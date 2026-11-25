//! 单行输入（架构整理 U12，用户决定 Q8）：backspace 按字素删除，每个单行输入都收粘贴（换行压成空格）。
//! 本分支：`/export` 的文件名、详细视图的搜索与询问框的意见没有单行输入（文件名在输入框里打，询问的意见进真正的输入
//! 框，按字素删），这里只测列表的过滤与输入历史的搜索。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::app::{App, Overlay};
use crate::ui::list::{ListView, Purpose};
use crate::ui::test_support;
use crate::ui::test_support::key;

/// 一个字素、两个字符：e 加组合的锐音符。
const ACCENTED: &str = "e\u{301}";
/// 一个字素、两个字符：竖大拇指加肤色。
const THUMBS: &str = "\u{1f44d}\u{1f3fd}";

#[test]
fn backspace_removes_one_grapheme_and_paste_flattens_lines() {
    let mut query = format!("a{ACCENTED}");
    assert!(super::backspace(&mut query));
    assert_eq!(query, "a");
    let mut query = "搜索".to_owned();
    super::backspace(&mut query);
    assert_eq!(query, "搜");
    let mut empty = String::new();
    assert!(!super::backspace(&mut empty));
    let mut line = "x".to_owned();
    super::paste(&mut line, "one\r\ntwo\nthree\rfour");
    assert_eq!(line, "xone two three four");
}

fn app() -> App {
    let mut app = test_support::app();
    app.composer.skills_loaded = true;
    app
}

fn backspace(app: &mut App) {
    app.on_key(key(KeyCode::Backspace), Instant::now());
}

fn filtered_list() -> ListView {
    ListView::new(Purpose::Info, "Pick".into(), Vec::new(), "none".into()).filterable()
}

fn list_filter(app: &App) -> &str {
    match &app.overlay {
        Some(Overlay::List(list)) => list.filter.as_deref().unwrap_or_default(),
        other => panic!("not a list: {other:?}"),
    }
}

/// 修前 backspace 是 `String::pop`，删掉一个字符：组合字符与带肤色的表情只删了一半。
#[test]
fn backspace_deletes_a_whole_grapheme_in_every_single_line_input() {
    // 列表的过滤。
    let mut app = app();
    let mut list = filtered_list();
    list.filter = Some(format!("a{THUMBS}"));
    app.overlay = Some(Overlay::List(list));
    backspace(&mut app);
    assert_eq!(list_filter(&app), "a");
    // 输入历史的搜索（ctrl+r）。
    app.overlay = None;
    app.on_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    for c in format!("a{ACCENTED}").chars() {
        app.on_key(key(KeyCode::Char(c)), Instant::now());
    }
    backspace(&mut app);
    assert_eq!(
        app.composer.search.as_ref().map(|s| s.0.as_str()),
        Some("a")
    );
}

/// 修前列表的过滤与输入历史的搜索不收粘贴（丢掉，或者进了被盖住的输入框）。
#[test]
fn every_single_line_input_takes_a_paste() {
    let mut app = app();
    app.overlay = Some(Overlay::List(filtered_list()));
    app.on_paste("glm\n5");
    assert_eq!(list_filter(&app), "glm 5");
    app.overlay = None;
    app.on_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    app.on_paste("git\npush");
    assert_eq!(
        app.composer.search.as_ref().map(|s| s.0.as_str()),
        Some("git push")
    );
    assert!(app.composer.is_empty(), "{:?}", app.composer.area.text());
}

/// 词的边界（tui-input.md §5）：字母与数字连成一段是词，别的是分隔；先跳过分隔再跳过词；按字素走，组合的重音符随前面
/// 的字母，不会停在字素中间。
#[test]
fn word_bounds_skip_separators_then_the_word_by_grapheme() {
    use super::{word_end, word_start};
    let text = "git push --force-with-lease";
    assert_eq!(word_start(text, text.len()), "git push --force-with-".len());
    assert_eq!(word_start(text, "git push --".len()), "git ".len());
    assert_eq!(word_end(text, "git".len()), "git push".len());
    assert_eq!(word_end(text, "git push".len()), "git push --force".len());
    assert_eq!(word_start("  ", 2), 0);
    assert_eq!(word_end("ab", 2), 2);
    // `e` 加组合的锐音符是一个字素：词尾在它后面，不在两者之间（修前按字符走停在中间）。
    let accented = format!("ab{ACCENTED}c d");
    assert_eq!(word_end(&accented, 0), format!("ab{ACCENTED}c").len());
    assert_eq!(word_start(&accented, format!("ab{ACCENTED}c").len()), 0);
    // 中文是字母（`is_alphanumeric`）；带肤色的表情是分隔，整个跳过。
    let mixed = format!("搜索 {THUMBS} ok");
    assert_eq!(word_start(&mixed, mixed.len()), mixed.len() - 2);
    assert_eq!(word_start(&mixed, mixed.len() - 3), 0);
}

#[test]
fn kill_word_drops_the_last_word_and_its_trailing_separators() {
    let mut text = "alpha beta  ".to_owned();
    assert!(super::kill_word(&mut text));
    assert_eq!(text, "alpha ");
    assert!(super::kill_word(&mut text));
    assert_eq!(text, "");
    assert!(!super::kill_word(&mut text));
}

/// 修前单行输入没有按词删除：ctrl / alt 加 ⌫ 只删一个字素，ctrl+w 什么都不做，alt+d 在列表的过滤里打出 `d`。
#[test]
fn every_single_line_input_kills_a_word() {
    let (c, a) = (KeyModifiers::CONTROL, KeyModifiers::ALT);
    // 列表的过滤。
    let mut app = app();
    let mut list = filtered_list();
    list.filter = Some("glm air".to_owned());
    app.overlay = Some(Overlay::List(list));
    app.on_key(KeyEvent::new(KeyCode::Backspace, c), Instant::now());
    assert_eq!(list_filter(&app), "glm ");
    app.on_key(KeyEvent::new(KeyCode::Char('d'), a), Instant::now());
    assert_eq!(list_filter(&app), "glm ");
    app.on_key(KeyEvent::new(KeyCode::Char('w'), c), Instant::now());
    assert_eq!(list_filter(&app), "");
    // 输入历史的搜索（ctrl+r）。
    app.overlay = None;
    app.on_key(KeyEvent::new(KeyCode::Char('r'), c), Instant::now());
    app.on_paste("cargo test");
    app.on_key(KeyEvent::new(KeyCode::Backspace, a), Instant::now());
    assert_eq!(
        app.composer.search.as_ref().map(|s| s.0.as_str()),
        Some("cargo ")
    );
    app.on_key(KeyEvent::new(KeyCode::Delete, c), Instant::now());
    assert_eq!(
        app.composer.search.as_ref().map(|s| s.0.as_str()),
        Some("cargo ")
    );
}
