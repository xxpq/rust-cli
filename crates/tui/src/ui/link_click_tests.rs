//! 回答里的链接单击打开（docs/specs/rust-cli/tui-select.md §10，照 Puffin docs/puffin-tui.md §4.4「链接」）：经 `draw::draw`
//! 画出带链接的回答，再按下、松开鼠标。修复前单击什么都不做（tui.md §9 登记过“点击网址不打开浏览器”）。
use std::time::Duration;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_tui_kit::render::width::string_width;

use super::*;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::transcript::Cache;

const URL: &str = "https://example.com/a";

/// 画好的一屏与它的 App：回答是 `answer`（Markdown），能打开链接时 `opens`。
struct Screen {
    app: App,
    cache: Cache,
    width: u16,
    lines: Vec<String>,
    styled: Vec<rust_cli_tui_kit::render::StyledLine>,
}

fn screen(answer: &str, width: u16, opens: bool) -> Screen {
    let mut app = crate::ui::test_support::app();
    let colors =
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))).with_foreground(Some((255, 255, 255)));
    app.ink = crate::ui::ink::Ink::chosen(
        colors.level,
        colors.foreground,
        colors.background,
        app.ink.choice,
    );
    app.session = Some("sess_links".into());
    app.links_open = opens;
    let messages = [
        Message::User(UserMessage {
            id: MessageId::new("u1"),
            content: vec![ContentBlock::Text { text: "go".into() }],
            origin: UserOrigin::Human,
        }),
        Message::Assistant(AssistantMessage {
            id: MessageId::new("a1"),
            content: vec![ContentBlock::Text {
                text: answer.into(),
            }],
            model: "scripted".into(),
            stop_reason: None,
            usage: None,
        }),
    ];
    for (index, message) in messages.into_iter().enumerate() {
        let seq = index as u64 + 1;
        app.view.apply(
            SequencedEvent {
                seq,
                revision: 0,
                event: KernelEvent::MessageDone { message },
            },
            seq * 1000,
        );
    }
    let mut s = Screen {
        app,
        cache: Cache::default(),
        width,
        lines: Vec::new(),
        styled: Vec::new(),
    };
    s.draw();
    s
}

impl Screen {
    fn draw(&mut self) {
        let now = self.app.started + Duration::from_secs(5);
        let frame = crate::ui::draw::draw(&mut self.app, &mut self.cache, now, 0, self.width, 30);
        self.lines = frame
            .lines
            .iter()
            .map(rust_cli_tui_kit::render::StyledLine::plain)
            .collect();
        self.styled = frame.lines;
    }

    /// 第 `y` 行第 `x` 格的底色。
    fn bg(&self, x: u16, y: u16) -> rust_cli_tui_kit::render::Color {
        let mut col = 0;
        for span in &self.styled[usize::from(y)].spans {
            let w = string_width(&span.text);
            if usize::from(x) < col + w {
                return span.style.bg_color();
            }
            col += w;
        }
        rust_cli_tui_kit::render::Color::Default
    }

    /// `needle` 第一次出现处的（列，行）。
    fn at(&self, needle: &str) -> (u16, u16) {
        let (y, line) = self
            .lines
            .iter()
            .enumerate()
            .find(|(_, line)| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} in {:#?}", self.lines));
        let x = string_width(&line[..line.find(needle).unwrap()]);
        (x as u16, y as u16)
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        (x, y): (u16, u16),
        modifiers: KeyModifiers,
    ) -> Option<Action> {
        self.app.on_mouse(
            MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers,
            },
            self.app.started + Duration::from_secs(6),
        )
    }

    /// 在 `at` 按下、松开（没有拖动）。
    fn click(&mut self, at: (u16, u16), modifiers: KeyModifiers) -> Option<Action> {
        let _ = self.mouse(MouseEventKind::Down(MouseButton::Left), at, modifiers);
        self.mouse(MouseEventKind::Up(MouseButton::Left), at, modifiers)
    }
}

fn opens(url: &str) -> Option<Action> {
    Some(Action::OpenLink(url.to_owned()))
}

#[test]
fn a_single_click_on_a_link_opens_it() {
    let mut s = screen(&format!("see [the docs]({URL}) now"), 80, true);
    let at = s.at("the docs");
    assert_eq!(s.click(at, KeyModifiers::NONE), opens(URL));
}

#[test]
fn a_link_wrapped_onto_two_rows_opens_from_either_piece() {
    let text = format!("see [one two three four five six]({URL}) now");
    let mut s = screen(&text, 22, true);
    let (first, second) = (s.at("one"), s.at("six"));
    assert_ne!(first.1, second.1, "{:#?}", s.lines);
    assert_eq!(s.click(first, KeyModifiers::NONE), opens(URL));
    assert_eq!(s.click(second, KeyModifiers::NONE), opens(URL));
}

#[test]
fn dragging_over_a_link_selects_its_words_instead() {
    let mut s = screen(&format!("see [the docs]({URL}) now"), 80, true);
    let (x, y) = s.at("the docs");
    let _ = s.mouse(
        MouseEventKind::Down(MouseButton::Left),
        (x, y),
        KeyModifiers::NONE,
    );
    let _ = s.mouse(
        MouseEventKind::Drag(MouseButton::Left),
        (x + 4, y),
        KeyModifiers::NONE,
    );
    let up = s.mouse(
        MouseEventKind::Up(MouseButton::Left),
        (x + 4, y),
        KeyModifiers::NONE,
    );
    assert_eq!(up, Some(Action::CopySelection { clear: false }));
}

#[test]
fn a_held_modifier_a_remote_shell_or_no_target_does_not_open() {
    let mut s = screen(&format!("see [the docs]({URL}) now"), 80, true);
    let at = s.at("the docs");
    for modifiers in [
        KeyModifiers::CONTROL,
        KeyModifiers::SHIFT,
        KeyModifiers::ALT,
        KeyModifiers::SUPER,
    ] {
        // 同一处连点会被算成双击、三击（选词、选行），这里只看它不打开。
        assert_ne!(s.click(at, modifiers), opens(URL), "{modifiers:?}");
    }
    // 远程 shell 或入口没给打开方式：链接不能点。
    let mut remote = screen(&format!("see [the docs]({URL}) now"), 80, false);
    let at = remote.at("the docs");
    assert_eq!(remote.click(at, KeyModifiers::NONE), None);
    // 没有协议的目标只有样子。
    let mut relative = screen("see [notes](docs/notes.md) now", 80, true);
    let at = relative.at("notes");
    assert_eq!(relative.click(at, KeyModifiers::NONE), None);
}

/// 链接优先于块（Puffin「链接优先于工作的收拢」）：同一格既是链接又在可展开的块里时打开链接，块不动。区域直接写进 `Hits`，
/// 当作上一帧画出来的。
#[test]
fn a_link_inside_an_expandable_block_opens_and_leaves_the_block_alone() {
    let mut app = crate::ui::test_support::app();
    app.links_open = true;
    let hits = &mut app.pointer.hits;
    hits.transcript = Rect::new(0, 0, 40, 10);
    hits.rows = vec![((0, 0), String::new()); 10];
    hits.rows[2] = ((7, 0), "   see the docs now".to_owned());
    hits.expandable = [7].into();
    hits.links = vec![Link {
        rect: Rect::new(7, 2, 8, 1),
        url: URL.to_owned(),
        key: 7,
    }];
    let now = app.started + Duration::from_secs(1);
    let event = |kind| MouseEvent {
        kind,
        column: 9,
        row: 2,
        modifiers: KeyModifiers::NONE,
    };
    let _ = app.on_mouse(event(MouseEventKind::Down(MouseButton::Left)), now);
    let up = app.on_mouse(event(MouseEventKind::Up(MouseButton::Left)), now);
    assert_eq!(up, opens(URL));
    assert!(!app.port.expanded_cells.contains(&7));
    // 块里不是链接的字：照常展开。
    let event = |kind| MouseEvent {
        kind,
        column: 4,
        row: 2,
        modifiers: KeyModifiers::NONE,
    };
    let later = now + Duration::from_secs(1);
    let _ = app.on_mouse(event(MouseEventKind::Down(MouseButton::Left)), later);
    let _ = app.on_mouse(event(MouseEventKind::Up(MouseButton::Left)), later);
    assert!(app.port.expanded_cells.contains(&7));
}

/// 鼠标移到链接上（§9.1）：重画后这个链接的格子铺上悬停的底，别的字不铺；不能打开时不铺。
#[test]
fn hovering_a_link_lights_its_cells() {
    let mut s = screen(&format!("see [the docs]({URL}) now"), 80, true);
    let (x, y) = s.at("the docs");
    let _ = s.mouse(MouseEventKind::Moved, (x + 1, y), KeyModifiers::NONE);
    s.draw();
    let hover = s.app.ink.hover_bg().expect("rich theme");
    assert_eq!(s.bg(x, y), hover);
    assert_eq!(s.bg(x + 7, y), hover);
    assert_ne!(
        s.bg(x - 2, y),
        hover,
        "the words before the link are not lit"
    );
    let mut off = screen(&format!("see [the docs]({URL}) now"), 80, false);
    let (x, y) = off.at("the docs");
    let _ = off.mouse(MouseEventKind::Moved, (x + 1, y), KeyModifiers::NONE);
    off.draw();
    assert_ne!(off.bg(x, y), hover);
}
