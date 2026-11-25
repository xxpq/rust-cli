//! 设置页随界面语言（docs/specs/rust-cli/tui-kit.md §4.5「语言」，用户决定 2026-11-12）：中文界面里段名、行名、值、说明、
//! 占位与状态栏的键都是中文；英文界面与以前一字不差；搜索两种写法都认。
use std::time::Instant;

use rust_cli_tui_kit::render::StyledLine;

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::test_support::{app, app_in};

fn screen(app: &mut App, width: usize, rows: usize) -> String {
    app.settings_frame(Instant::now(), 0, width, rows)
        .expect("the settings")
        .iter()
        .map(|l| StyledLine::plain(l).trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 中文界面：标题、段目录、段的说明、搜索框的占位与状态栏的键。
#[test]
fn the_directory_and_the_keys_follow_the_chinese_screen() {
    let mut app = app_in(Lang::Zh);
    let _ = app.slash("/settings");
    let text = screen(&mut app, 100, 40);
    for word in [
        "设置",
        "常规",
        "权限",
        "模型",
        "外观",
        "子代理",
        "机器",
        "技能",
        "状态",
        "上下文",
        "统计",
        "代理怎样工作",
        "搜索设置",
        "↑↓ 段",
        "→ 打开",
        "esc 关闭",
    ] {
        assert!(text.contains(word), "{word}:\n{text}");
    }
    for word in ["General", "Appearance", "Search settings", "close"] {
        assert!(!text.contains(word), "{word}:\n{text}");
    }
}

/// 中文界面的行：行名、值、选中行下面的说明与生效的时刻；主题名不译。
#[test]
fn rows_values_and_descriptions_are_chinese() {
    let mut app = app_in(Lang::Zh);
    let _ = app.slash("/settings appearance");
    let text = screen(&mut app, 100, 40);
    for word in [
        "主题",
        "明暗",
        "背景",
        "自动",
        "默认",
        "屏幕的颜色。",
        "立即显示。",
    ] {
        assert!(text.contains(word), "{word}:\n{text}");
    }
    assert!(!text.contains("Light or dark"), "{text}");
}

/// 英文界面与以前相同。
#[test]
fn the_english_screen_is_unchanged() {
    let mut app = app();
    let _ = app.slash("/settings");
    let text = screen(&mut app, 100, 40);
    for word in [
        "Settings",
        "General",
        "Appearance",
        "Search settings",
        "↑↓ section  → open  esc close",
    ] {
        assert!(text.contains(word), "{word}:\n{text}");
    }
    assert!(!text.contains("常规"), "{text}");
}

/// 搜索两种写法都认，显示当前语言的行；`/settings <段名>` 也认中文段名。
#[test]
fn search_and_section_names_work_in_both_languages() {
    for query in ["theme", "主题"] {
        let mut app = app_in(Lang::Zh);
        let _ = app.slash(&format!("/settings {query}"));
        assert!(app.settings.as_ref().unwrap().searching(), "{query}");
        let text = screen(&mut app, 100, 40);
        assert!(text.contains("主题"), "{query}:\n{text}");
        assert!(!text.contains("没有找到"), "{query}:\n{text}");
    }
    let mut app = app_in(Lang::Zh);
    let _ = app.slash("/settings 外观");
    let ui = app.settings.as_ref().unwrap();
    assert!(!ui.searching());
    assert_eq!(page::section_of(&ui.page()), "appearance");
}
