//! 页的一屏（`page_frame`，tui-kit.md §4.0「页」）：正文换成页、底部栈留着；看不见的行写在第一与最后一行；页的键在槽里。
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::page_frame::PageBody;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

fn app() -> App {
    App::new(
        Colors::new(ColorLevel::TrueColor, Some((21, 22, 26)))
            .with_foreground(Some((214, 216, 220))),
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

fn texts(lines: &[StyledLine]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

#[test]
fn a_page_replaces_the_body_and_keeps_the_bottom_stack() {
    let mut app = app();
    let page = PageBody {
        lines: (0..40)
            .map(|i| StyledLine::new(vec![Span::plain(format!("  line {i}"))]))
            .collect(),
        top: 5,
        keys: vec![Span::plain("esc back")],
        owns_keys: true,
        fit: false,
    };
    let now = app.started;
    let screen = page_frame(&mut app, &page, now, 0, 60, 20);
    let rows = texts(&screen);
    assert_eq!(rows.len(), 20);
    // 顶上一行说上面还有几行，底部栈上面那一行说下面还有几行。
    assert_eq!(rows[0], "   ↑ 6 more");
    let board = rows.iter().position(|r| r.starts_with('▄')).unwrap();
    assert!(rows[board - 1].starts_with("   ↓ "), "{rows:#?}");
    assert_eq!(rows[1], "  line 6");
    // 板与状态栏留着；槽空着，拿键时没有占位句（tui-kit.md §4.0「页的键」）。
    assert_eq!(rows[board + 1].trim(), "›", "{rows:#?}");
    assert!(rows[board + 2].starts_with('▀'), "{rows:#?}");
    // 状态栏是最后两行（tui-display.md §2.6）：第 1 行左边是模式、右边是页的键（数字那组不写），第 2 行是目录。
    assert!(rows[18].starts_with("   Ask before changes"), "{rows:#?}");
    assert!(rows[18].ends_with("esc back"), "{rows:#?}");
    assert!(rows[19].starts_with("   ~/project"), "{rows:#?}");
}

/// 页里的网址（tui-select.md §10「页里的网址」，用户决定 2026-11-12）：页的行里带链接的字单击打开、悬停铺底；跨软折行的
/// 几段同一条网址。修复前页不记链接与可点击区域，单击什么都不做。
mod links {
    use std::time::Duration;

    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use rust_cli_tui_kit::render::width::string_width;
    use rust_cli_tui_kit::render::{Span, StyledLine};

    use super::{app, texts};
    use crate::ui::app::{Action, App};
    use crate::ui::draw::page_frame;
    use crate::ui::page_frame::PageBody;

    const URL: &str = "https://example.com/page/7";

    fn page() -> PageBody {
        let mut lines = vec![
            StyledLine::new(vec![Span::plain("  Usage")]),
            StyledLine::new(vec![Span::plain(format!("  see {URL} for more"))]),
        ];
        crate::ui::links::mark(&mut lines);
        PageBody {
            lines,
            top: 0,
            keys: vec![Span::plain("esc back")],
            owns_keys: true,
            fit: false,
        }
    }

    /// 画一屏，返回网址的（列，行）。
    fn draw(app: &mut App, page: &PageBody) -> (u16, u16, Vec<StyledLine>) {
        let screen = page_frame(app, page, app.started, 0, 60, 20);
        let rows = texts(&screen);
        let y = rows.iter().position(|r| r.contains(URL)).expect("the url");
        let x = string_width(&rows[y][..rows[y].find(URL).unwrap()]) + 2;
        (x as u16, y as u16, screen)
    }

    fn mouse(app: &mut App, kind: MouseEventKind, (x, y): (u16, u16)) -> Option<Action> {
        app.on_mouse(
            MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            },
            app.started + Duration::from_secs(1),
        )
    }

    fn click(app: &mut App, at: (u16, u16)) -> Option<Action> {
        mouse(app, MouseEventKind::Down(MouseButton::Left), at);
        mouse(app, MouseEventKind::Up(MouseButton::Left), at)
    }

    #[test]
    fn a_url_on_a_page_opens_on_a_click_and_lights_on_hover() {
        let mut app = app();
        app.links_open = true;
        let page = page();
        let (x, y, _) = draw(&mut app, &page);
        assert_eq!(
            click(&mut app, (x, y)),
            Some(Action::OpenLink(URL.to_owned()))
        );
        // 悬停：鼠标移到网址上，下一帧那几格铺悬停的底色。
        mouse(&mut app, MouseEventKind::Moved, (x, y));
        let (_, _, screen) = draw(&mut app, &page);
        let hover = app.ink.hover_bg().expect("a hover ground");
        let lit = screen[usize::from(y)]
            .spans
            .iter()
            .find(|s| s.text.contains("example"))
            .expect("the url span");
        assert_eq!(lit.style.bg_color(), hover);
        // 网址以外的字不能点。
        assert_eq!(click(&mut app, (3, y)), None);
    }

    #[test]
    fn without_a_way_to_open_the_page_urls_are_not_spots() {
        let mut app = app();
        app.links_open = false;
        let page = page();
        let (x, y, _) = draw(&mut app, &page);
        assert_eq!(click(&mut app, (x, y)), None);
    }
}
