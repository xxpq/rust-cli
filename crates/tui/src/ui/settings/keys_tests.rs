//! 设置页的文字键（tui-input.md §5「按词删除与移动」）：值与搜索用和输入框同一套删词的键与词的边界。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::field::Field;
use super::super::{Mode, Settings};
use crate::ui::app::App;
use crate::ui::test_support::app;

fn editing(text: &str) -> App {
    let mut app = app();
    app.settings = Some(Settings {
        mode: Mode::Editing {
            key: "value".into(),
            draft: Field::at_end(text),
        },
        ..Settings::default()
    });
    app
}

fn draft(app: &App) -> (String, usize) {
    match app.settings.as_ref().map(|ui| &ui.mode) {
        Some(Mode::Editing { draft, .. }) => (draft.text.clone(), draft.cursor),
        other => panic!("not editing: {other:?}"),
    }
}

fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let _ = app.settings_key(KeyEvent::new(code, modifiers));
}

/// 修前 ctrl+⌫ 只删一个字符，alt+d 打出 `d`，没有删后一个词；词按空白分（`example.com/v1` 一次删光）。
#[test]
fn the_value_kills_words_like_the_input_box() {
    let (c, a) = (KeyModifiers::CONTROL, KeyModifiers::ALT);
    let mut app = editing("https://example.com/v1");
    press(&mut app, KeyCode::Backspace, c);
    assert_eq!(draft(&app), ("https://example.com/".into(), 20));
    press(&mut app, KeyCode::Backspace, a);
    assert_eq!(draft(&app), ("https://example.".into(), 16));
    press(&mut app, KeyCode::Char('w'), c);
    assert_eq!(draft(&app), ("https://".into(), 8));
    press(&mut app, KeyCode::Home, KeyModifiers::NONE);
    press(&mut app, KeyCode::Delete, c);
    assert_eq!(draft(&app), ("://".into(), 0));
    let mut app = editing("one two");
    press(&mut app, KeyCode::Left, c);
    assert_eq!(draft(&app), ("one two".into(), 4));
    press(&mut app, KeyCode::Char('d'), a);
    assert_eq!(draft(&app), ("one ".into(), 4));
    press(&mut app, KeyCode::Left, a);
    press(&mut app, KeyCode::Delete, a);
    assert_eq!(draft(&app), (" ".into(), 0));
    // ctrl+h 不变：仍是 ⌫。
    let mut app = editing("ab");
    press(&mut app, KeyCode::Char('h'), c);
    assert_eq!(draft(&app), ("a".into(), 1));
}

/// 修前搜索里 ctrl+w 与 alt+⌫ 什么都不做，ctrl+⌫ 只删一个字符。
#[test]
fn the_search_kills_the_last_word() {
    let mut app = app();
    app.settings = Some(Settings {
        search: Field::at_end("theme dark"),
        ..Settings::default()
    });
    let search = |app: &App| app.settings.as_ref().map(|ui| ui.search.text.clone());
    press(&mut app, KeyCode::Backspace, KeyModifiers::CONTROL);
    assert_eq!(search(&app).as_deref(), Some("theme "));
    press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert_eq!(search(&app).as_deref(), Some(""));
}
