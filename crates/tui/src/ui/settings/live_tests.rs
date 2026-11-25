//! 设置的三段 Status、Context（Details）、Stats（docs/specs/rust-cli/tui-panels.md §4）：段在目录的第三组、不进搜索；内容排在
//! 窗格里（去掉以前页框的 3 列左边距）；滚动按页记、夹到最后一屏，滚轮也滚；Details 跟着选中滚。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_tui_kit::app::widgets::settings::SECTIONS;
use rust_cli_tui_kit::render::{Span, Style, StyledLine};

use super::*;
use crate::ui::test_support::app;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn screen(app: &mut App, width: u16, rows: u16) -> Vec<String> {
    app.settings_frame(Instant::now(), 0, usize::from(width), usize::from(rows))
        .expect("the settings")
        .iter()
        .map(|l| StyledLine::plain(l).trim_end().to_owned())
        .collect()
}

/// 三段在目录的最后一组（Hooks 之后空一行），次序 Status、Context、Stats。
#[test]
fn the_three_sections_are_the_last_group() {
    let last: Vec<&str> = SECTIONS
        .iter()
        .filter(|s| s.group == 2)
        .map(|s| s.id)
        .collect();
    assert_eq!(last, ["status", "context", "stats"]);
    assert_eq!(SECTIONS[SECTIONS.len() - 4].id, "hooks");
    let mut app = app();
    let _ = app.slash("/settings");
    let text = screen(&mut app, 100, 40);
    let hooks = text
        .iter()
        .position(|l| l.trim_start().starts_with("Hooks"))
        .unwrap();
    assert_eq!(text[hooks + 1].trim(), "", "{text:#?}");
    assert!(
        text[hooks + 2].trim_start().starts_with("Status"),
        "{text:#?}"
    );
}

/// 搜索只找设置的行：三段没有命中，也不因为没读到数据而说 `Reading…`。
#[test]
fn search_skips_the_read_only_sections() {
    let mut app = app();
    // 段的 id 打开那一段（同别的段）。
    let _ = app.slash("/settings status");
    assert_eq!(app.settings_live(), Some(Live::Status));
    // 别的词是搜索：搜索时不是只读的页。
    let _ = app.slash("/settings session");
    let ui = app.settings.as_ref().unwrap();
    assert!(ui.searching());
    assert!(app.settings_live().is_none());
    assert!(
        app.settings_hits(ui, 0)
            .iter()
            .all(|(s, _)| !matches!(*s, "status" | "context" | "stats"))
    );
}

/// 页框的左边距去掉 3 列：内容从窗格的第 0 列起（屏上第 16 列，目录之后）。
#[test]
fn the_content_starts_at_the_pane_column() {
    let mut app = app();
    let _ = app.slash("/status");
    let text = screen(&mut app, 100, 40);
    let session = text
        .iter()
        .find(|l| l.contains("Session    not started"))
        .unwrap_or_else(|| panic!("{text:#?}"));
    assert_eq!(
        session.find("Session    not started"),
        Some(16),
        "{session}"
    );
    // 窄于 76 列没有目录：窗格从第 3 列起。
    let text = screen(&mut app, 70, 40);
    let session = text
        .iter()
        .find(|l| l.contains("Session    not started"))
        .unwrap_or_else(|| panic!("{text:#?}"));
    assert_eq!(session.find("Session"), Some(3), "{session}");
    // 窄时状态栏写 `←→ section`。
    // 状态栏第 1 行是倒数第 2 行（最后一行是目录与标题，tui-display.md §2.6）。
    assert!(
        text.iter().rev().nth(1).unwrap().contains("←→ section"),
        "{text:#?}"
    );
}

/// 去边距只去行首的 3 个空格；不是空格时到那里为止。
#[test]
fn unlead_drops_only_leading_spaces() {
    let line = |spans: &[&str]| {
        StyledLine::new(
            spans
                .iter()
                .map(|t| Span::new((*t).to_owned(), Style::DEFAULT))
                .collect(),
        )
    };
    let out = unlead(vec![
        line(&["  ", " x", "  y"]),
        line(&["ab"]),
        line(&["     z"]),
    ]);
    let text: Vec<String> = out.iter().map(StyledLine::plain).collect();
    assert_eq!(text, ["x  y", "ab", "  z"]);
}

/// 让选中的行看得见：离一屏的上下边至少一行，夹到最后一屏。
#[test]
fn the_details_follow_the_choice_with_a_margin() {
    assert_eq!(follow(0, 5, 10, 40), 0);
    assert_eq!(follow(0, 9, 10, 40), 1);
    assert_eq!(follow(10, 10, 10, 40), 9);
    assert_eq!(follow(0, 39, 10, 40), 30);
    assert_eq!(follow(20, 0, 10, 40), 0);
}

/// 顶行按页记：滚了 Status 再去 Stats 从头，回来也从头（记的是别的页）；滚轮滚只读的段，别的段不动。
#[test]
fn the_top_belongs_to_one_page_and_the_wheel_scrolls() {
    let mut app = app();
    app.machines_enabled = false;
    let _ = app.slash("/status");
    // 矮的终端：内容比正文高。
    let _ = screen(&mut app, 100, 12);
    app.settings_key(key(KeyCode::Down));
    assert_eq!(app.settings.as_ref().unwrap().top(), 1);
    let _ = app.wheel(true);
    assert!(app.settings.as_ref().unwrap().top() > 1);
    let _ = app.slash("/stats");
    assert_eq!(app.settings.as_ref().unwrap().top(), 0);
    let _ = app.slash("/settings");
    let _ = app.wheel(true);
    assert_eq!(app.settings.as_ref().unwrap().top(), 0);
}
