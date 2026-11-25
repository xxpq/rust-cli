//! 关上设置页时说改了什么（docs/specs/rust-cli/tui-kit.md §4.5「关上时说改了什么」，用户决定 2026-11-12）：改的当时
//! 不说成功；关上时与打开时比，变了的值每项一行，作为本地命令的结果；改回去的与只换了界面语言的不算。
use rust_cli_tui_kit::theme::scheme::{ThemeChoice, ThemeMode};

use super::*;
use crate::ui::app::Local;
use crate::ui::i18n::Lang;
use crate::ui::test_support::{app, app_in};

fn dark(app: &App) -> ThemeChoice {
    ThemeChoice {
        mode: ThemeMode::Dark,
        ..app.ink.choice
    }
}

/// 一项：改的当时输入框上方不说成功；关上时是安静的一行。
#[test]
fn one_change_is_said_when_the_page_closes() {
    let mut app = app();
    let was = app.ink.choice;
    let _ = app.slash("/settings appearance");
    let _ = app.settings_run("themeMode", Act::Look(dark(&app)));
    assert_eq!(app.settings.as_ref().unwrap().said, None);
    app.settings_close();
    assert_eq!(app.said.quiet.as_deref(), Some("Set light or dark to Dark"));
    let _ = was;
}

/// 两项：多行，进 transcript 的本地块，按段与行的次序（General 在 Appearance 之前）；开关写 `Disabled …`。General 的行在段读到之后才有（之后才记原值）。
#[test]
fn several_changes_are_one_block() {
    let mut app = app();
    let _ = app.slash("/settings appearance");
    app.settings_read(Ok(SectionData::General(data::General {
        auto_compact: None,
        locale: "auto".to_owned(),
    })));
    let _ = app.settings_run("themeMode", Act::Look(dark(&app)));
    let _ = app.settings_run("leftArrowOpensCockpit", Act::LeftArrow(false));
    assert_eq!(app.settings.as_ref().unwrap().said, None);
    app.settings_close();
    let last = app.local.last().map(|cell| cell.local.clone());
    assert_eq!(
        last,
        Some(Local::Info(
            "Disabled ← opens the task list\nSet light or dark to Dark".to_owned()
        ))
    );
}

/// 改回去的不说；没有改的什么都不说（用户决定 2026-11-12）。
#[test]
fn a_change_put_back_or_nothing_changed_says_nothing() {
    let mut app = app();
    let was = app.ink.choice;
    let _ = app.slash("/settings appearance");
    let _ = app.settings_run("themeMode", Act::Look(dark(&app)));
    let _ = app.settings_run("themeMode", Act::Look(was));
    app.settings_close();
    assert_eq!(app.said.quiet, None);
    assert!(app.local.is_empty());
    let _ = app.slash("/settings");
    app.settings_close();
    assert_eq!(app.said.quiet, None);
}

/// 中文：`已将明暗设为深色`；只换了界面语言不算改了别的设置（比的是原始的值）。
#[test]
fn chinese_lines_and_a_language_switch_alone() {
    let mut app = app_in(Lang::Zh);
    let _ = app.slash("/settings appearance");
    let _ = app.settings_run("themeMode", Act::Look(dark(&app)));
    app.settings_close();
    assert_eq!(app.said.quiet.as_deref(), Some("已将明暗设为深色"));
    let mut app = app_in(Lang::En);
    let _ = app.slash("/settings appearance");
    app.lang = Lang::Zh;
    app.settings_close();
    assert_eq!(app.said.quiet, None);
}
