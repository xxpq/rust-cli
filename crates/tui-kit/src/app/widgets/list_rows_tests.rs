//! 输入框上方的列表、命令架、栈、状态栏与 `/stats` 的卡（docs/specs/rust-cli/tui-kit.md §4.4 K4）：每个从 Puffin 搬来的画法
//! 一个有代表性的中性输入，铺出来看行。Puffin 的 picker、modes、resume、rewind、key_hints、status_line、stats 的测试经它的
//! 映射测同样的规则；时间的说法与改动的纯用例随画法从 Puffin 搬来。
use crate::app::element::{Element, stack};
use crate::app::layout::Layout;
use crate::app::widgets::key_hints::{self, Slot};
use crate::app::widgets::model_picker::{self, Model, Picker};
use crate::app::widgets::modes::{self, Mode};
use crate::app::widgets::resume_list::{self, Changes, Mark, Session};
use crate::app::widgets::rewind_rows::{self, Point, PointFrom};
use crate::app::widgets::stats::{self, Usage, Window};
use crate::app::widgets::status_bar::{self, Left};
use crate::app::widgets::{export_list, lists, queue_stack, shelf, theme_list, when};
use crate::render::width::string_width;
use crate::render::{Span, Style};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

const NOW: f64 = 1_792_080_000_000.0;
const HOUR: f64 = 3_600_000.0;

fn rich() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

fn utc(_: f64) -> i32 {
    0
}

fn lines(rows: Vec<Element>, w: usize) -> Vec<String> {
    Layout::lay(&stack(rows), w)
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

fn text(spans: &[Span]) -> String {
    spans.iter().map(|sp| sp.text.as_ref()).collect()
}

#[test]
fn a_window_follows_the_choice_and_the_filter_lights_what_matched() {
    // 一端有藏起的行时它占一行 more：显示 rows - 1 行。
    assert_eq!(lists::window(20, 0, 5), (0, 4));
    assert_eq!(lists::window(20, 19, 5), (16, 20));
    assert_eq!(lists::window(3, 2, 5), (0, 3));
    let t = rich();
    let lit = lists::matched(&t, "glm-5", "5");
    assert_eq!(text(&lit), "glm-5");
    assert!(
        lit.iter()
            .any(|s| s.text == "5" && s.style == Style::DEFAULT)
    );
}

#[test]
fn the_shelf_lists_commands_a_greyed_one_says_why() {
    assert_eq!(shelf::word("/re"), Some("re"));
    assert!(shelf::matches("quit", &[("exit", "quit")], "ex"));
    assert!(!shelf::matches("quit", &[("exit", "quit")], "zz"));
    let rows = [
        shelf::ShelfRow {
            name: "resume",
            description: "Pick up an earlier session",
            unavailable: None,
        },
        shelf::ShelfRow {
            name: "rewind",
            description: "Go back to a message",
            unavailable: Some("Nothing to go back to yet"),
        },
    ];
    let got = lines(shelf::rows(&rich(), &rows, 0), 100);
    assert!(got[0].contains("/resume") && got[0].contains("Pick up an earlier session"));
    assert!(got.iter().any(|l| l.contains("Nothing to go back to yet")));
}

#[test]
fn the_stack_shows_the_queued_messages_under_its_label() {
    let got = lines(queue_stack::rows(&rich(), &["first", "second"], 80), 80);
    assert!(got.iter().any(|l| l.contains(queue_stack::STACK_LABEL)));
    assert!(got.iter().any(|l| l.contains("first")));
    assert!(got.iter().any(|l| l.contains("second")));
}

#[test]
fn the_model_picker_lists_models_by_provider_then_the_efforts() {
    let models = [
        Model {
            provider_id: "zai",
            provider_name: "Z.ai",
            model_id: "glm-5",
            current: true,
        },
        Model {
            provider_id: "zai",
            provider_name: "Z.ai",
            model_id: "glm-4.6",
            current: false,
        },
    ];
    let efforts = ["low".to_owned(), "high".to_owned()];
    let p = Picker {
        any: true,
        widest_provider: 4,
        matches: &models,
        open: true,
        choice: 0,
        filter: "",
        efforts: &efforts,
        level: Some(1),
        named_age: 100,
    };
    let got = lines(model_picker::rows(&rich(), &p, 100), 100);
    assert!(
        got.iter()
            .any(|l| l.contains("glm-5") && l.contains("Z.ai"))
    );
    assert!(got.iter().any(|l| l.contains("glm-4.6")));
    assert!(model_picker::is_off("none") && !model_picker::is_off("high"));
}

#[test]
fn the_mode_list_marks_the_current_mode() {
    assert_eq!(modes::label(Mode::Yolo), "Full access");
    let got = lines(
        modes::list_rows(&rich(), 0, Some((Mode::Build, false))),
        100,
    );
    assert_eq!(got.len(), modes::MODE_ROWS.len());
    assert!(got.iter().any(|l| l.contains("Full access")));
}

#[test]
fn the_theme_list_filters_by_name() {
    let all = theme_list::matches("");
    assert!(all.len() > 1);
    let first = all[0];
    let got = lines(theme_list::rows(&rich(), Some(first), first, ""), 100);
    assert!(got.iter().any(|l| l.contains(first)));
    assert!(theme_list::matches("zzzz-no-theme").is_empty());
}

#[test]
fn the_export_list_has_its_rows() {
    let got = lines(export_list::rows(&rich(), 0), 100);
    assert_eq!(got.len(), export_list::EXPORT_ROWS.len());
}

#[test]
fn ages_days_for_conversations_a_clock_in_the_preview() {
    use crate::app::widgets::when::{day_age, day_age_long};
    assert_eq!(day_age(NOW, NOW - HOUR, &utc), "today");
    assert_eq!(day_age(NOW, NOW - 20.0 * HOUR, &utc), "yesterday");
    assert_eq!(day_age(NOW, NOW - 72.0 * HOUR, &utc), "3 days");
    assert_eq!(day_age(NOW, NOW - 70.0 * 24.0 * HOUR, &utc), "2 months");
    assert_eq!(day_age_long(NOW, 1_792_055_100_000.0, &utc), "today, 09:05");
    assert_eq!(day_age_long(NOW, NOW - 72.0 * HOUR, &utc), "3 days ago");
    assert_eq!(when::hh_mm(1_792_055_100_000.0, &utc), "09:05");
}

#[test]
fn the_resume_list_shows_each_session_with_its_age_and_changes() {
    let s = Session {
        title: "Fix the login".to_owned(),
        created_at: NOW - 3.0 * HOUR,
        updated_at: NOW - HOUR,
        model: "glm-5",
        effort: "high",
        mark: Mark::Unread,
        changes: Some(Changes {
            added: 12,
            removed: 3,
            files: 2,
        }),
        snippet: None,
        folder: None,
        current: false,
        dash: None,
    };
    let matches = [s];
    let list = resume_list::List {
        earlier: true,
        matches: &matches,
        choice: 0,
        query: "",
        named_age: 100,
    };
    let got = lines(resume_list::rows(&rich(), &list, 100, NOW, &utc), 100);
    assert!(got.iter().any(|l| l.contains("Fix the login")));
    let none = resume_list::List {
        earlier: false,
        ..list
    };
    assert_eq!(
        lines(resume_list::rows(&rich(), &none, 100, NOW, &utc), 100),
        ["   No earlier sessions in this folder."]
    );
}

fn points() -> Vec<Point> {
    vec![
        Point {
            at: NOW as u64 - 3_600_000,
            from: PointFrom::Start,
            added: 0,
            removed: 0,
        },
        Point {
            at: NOW as u64 - 1_800_000,
            from: PointFrom::Answer,
            added: 27,
            removed: 2,
        },
    ]
}

#[test]
fn rewind_lists_your_messages_newest_first() {
    assert_eq!(rewind_rows::ordinal(22), "22nd");
    assert_eq!(rewind_rows::where_(&points(), 0), "the start");
    let texts = ["Plan the trip", "Book a hotel in Tokyo"];
    let words = |k: usize| texts[k].to_owned();
    assert_eq!(rewind_rows::matches(&points(), &words, "tokyo"), vec![1]);
    let ui = rewind_rows::List {
        query: "",
        choice: 0,
    };
    let got = lines(
        rewind_rows::list_rows(&rich(), &points(), &words, &ui, NOW, &utc),
        100,
    );
    let tokyo = got.iter().position(|l| l.contains("Tokyo"));
    let trip = got.iter().position(|l| l.contains("Plan the trip"));
    assert!(tokyo.is_some() && trip.is_some() && tokyo < trip);
}

#[test]
fn change_shows_only_the_parts_there_are() {
    let theme = rich();
    let t = |a, r| text(&rewind_rows::change(&theme, a, r));
    assert_eq!(t(27, 2), "+27 −2");
    assert_eq!(t(5, 0), "+5");
    assert_eq!(t(0, 3), "−3");
    assert_eq!(t(0, 0), "");
}

#[test]
fn the_slot_says_its_keys_and_the_sheet_lists_the_groups() {
    let t = rich();
    let slot = Slot::keys(&[("⏎", "send"), ("esc", "close")]);
    let said = text(&key_hints::slot_spans(&t, &slot, 100));
    assert!(said.contains("send") && said.contains("close"));
    let sheet = lines(
        key_hints::sheet(
            &t,
            key_hints::newline_key(false),
            key_hints::paste_key(),
            120,
        ),
        120,
    );
    assert!(sheet.iter().any(|l| l.contains("new line")));
}

/// 状态栏总是一行（tui-display.md §2.6，2026-11-12）：项之间是 ` · `，右端是速度、缓存、上下文、模型与档位；窄时名次低的先走
/// （机器、`⇧⇥`、速度、缓存、档位），不分成两行。
#[test]
fn the_status_bar_is_one_line_and_drops_the_lowest_ranks_first_when_narrow() {
    let t = rich();
    let left = status_bar::left(
        &t,
        &Left {
            mode: Some((Mode::Build, false, 100)),
            goal: None,
            machines: Some("@box"),
        },
    );
    let window = Window {
        used: 20_000,
        max: 1_000_000,
        compact_at: None,
        breakdown: Vec::new(),
    };
    let figures = stats::Figures {
        speed: vec![Span::new("182 tok/s", t.fg2)],
        cache: Some(0.853),
        context: Some(&window),
        context_age: 100,
        cache_word: "cache",
        context_word: "ctx",
    };
    let mut right = stats::parts(&t, &figures);
    right.extend(status_bar::model_parts(&t, "glm-5", "high", 100));
    let wide = text(&status_bar::line(&left, &right, &[], 140, t.line));
    assert!(
        wide.starts_with("   Ask before changes ⇧⇥ · @box"),
        "{wide:?}"
    );
    assert!(
        wide.ends_with("182 tok/s · cache 85.3% · ctx 2.0% · glm-5 · high "),
        "{wide:?}"
    );
    // 窄：机器、`⇧⇥`、速度、缓存先走；上下文、模型留着。仍是一行。
    let narrow = text(&status_bar::line(&left, &right, &[], 60, t.line));
    assert!(
        !narrow.contains("@box") && !narrow.contains("tok/s"),
        "{narrow:?}"
    );
    assert!(
        narrow.contains("ctx 2.0%") && narrow.contains("glm-5"),
        "{narrow:?}"
    );
    assert!(string_width(&narrow) <= 60, "{narrow:?}");
}

/// 上下文的页在计数到达之前与模型没有窗口时画的 Context 一行（`stats::context_rows`）：条、用掉的数与窗口；没有窗口时说
/// 模型不说它的窗口。
#[test]
fn the_context_row_fills_its_bar_or_says_the_window_is_unknown() {
    let t = rich();
    let window = Window {
        used: 124_000,
        max: 200_000,
        compact_at: None,
        breakdown: Vec::new(),
    };
    let known = Usage {
        context: Some(window),
        last_call_tokens: None,
    };
    let rows = stats::context_rows(&t, &known, 100);
    let first = text(&rows[0]);
    assert!(first.starts_with("   Context"), "{first:?}");
    assert!(
        first.contains('█') && first.contains("124k of 200k"),
        "{first:?}"
    );
    let unknown = Usage {
        context: None,
        last_call_tokens: Some(125_000),
    };
    let row = text(&stats::context_rows(&t, &unknown, 100)[0]);
    assert!(
        row.contains("125k tokens   the model does not say its window"),
        "{row:?}"
    );
}
