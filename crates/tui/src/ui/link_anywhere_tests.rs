//! 别处的网址也能点（docs/specs/rust-cli/tui-select.md §10「别处的网址也能点」，用户决定 2026-11-12）：你的消息、工具的输出、
//! 输入框上方那一句里的网址单击打开、悬停铺底；画出来的一屏经 `draw::draw`，再按下、松开鼠标。修复前只有回答里的链接
//! 能点，这些地方单击什么都不做。
use std::time::Duration;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use rust_cli_kernel_api::ids::{MessageId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, TurnOrigin};
use rust_cli_kernel_api::tool::ToolOutput;
use rust_cli_tui_kit::render::width::string_width;
use serde_json::json;

use super::*;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::transcript::Cache;

const ASKED: &str = "https://example.com/issue/7";
const PRINTED: &str = "https://ci.example.com/run/42";
const SAID: &str = "https://example.com/said";

struct Screen {
    app: App,
    cache: Cache,
    lines: Vec<String>,
    seq: u64,
}

impl Screen {
    fn new(opens: bool) -> Screen {
        let mut app = crate::ui::test_support::app();
        let colors = Colors::new(ColorLevel::TrueColor, Some((0, 0, 0)))
            .with_foreground(Some((255, 255, 255)));
        app.ink = crate::ui::ink::Ink::chosen(
            colors.level,
            colors.foreground,
            colors.background,
            app.ink.choice,
        );
        app.session = Some("sess_links".into());
        app.links_open = opens;
        Screen {
            app,
            cache: Cache::default(),
            lines: Vec::new(),
            seq: 0,
        }
    }

    fn event(&mut self, event: KernelEvent) {
        self.seq += 1;
        self.app.view.apply(
            SequencedEvent {
                seq: self.seq,
                revision: 0,
                event,
            },
            self.seq * 1000,
        );
    }

    /// 一个回合：你的消息带网址，一次 Bash 打印一个网址（JSON 的引号里），然后一句回答。
    fn turn(&mut self) {
        self.event(KernelEvent::TurnStarted {
            turn: TurnId::new("t1"),
            command: None,
            origin: TurnOrigin::Input,
            started_at: 0,
            works: Vec::new(),
        });
        self.event(KernelEvent::MessageDone {
            message: Message::User(UserMessage {
                id: MessageId::new("u1"),
                content: vec![ContentBlock::Text {
                    text: format!("look at {ASKED} please"),
                }],
                origin: UserOrigin::Human,
            }),
        });
        let printed = format!("{{\"run\": \"{PRINTED}\"}}");
        self.event(KernelEvent::ToolStarted {
            tool_use: ToolUseId::new("tu1"),
            name: "Bash".into(),
            input: json!({"command": "ci status"}),
            started_at: 0,
        });
        self.event(KernelEvent::MessageDone {
            message: Message::Assistant(AssistantMessage {
                id: MessageId::new("a1"),
                content: vec![ContentBlock::ToolUse {
                    id: ToolUseId::new("tu1"),
                    name: "Bash".into(),
                    input: json!({"command": "ci status"}),
                }],
                model: "scripted".into(),
                stop_reason: None,
                usage: None,
            }),
        });
        self.event(KernelEvent::ToolDone {
            tool_use: ToolUseId::new("tu1"),
            output: ToolOutput {
                content: vec![ToolResultContent::Text {
                    text: printed.clone(),
                }],
                is_error: false,
                display: serde_json::Value::Null,
            },
            cancelled: false,
        });
        self.event(KernelEvent::MessageDone {
            message: Message::User(UserMessage {
                id: MessageId::new("r1"),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: ToolUseId::new("tu1"),
                    content: vec![ToolResultContent::Text { text: printed }],
                    is_error: false,
                }],
                origin: UserOrigin::ToolResult {
                    assistant: MessageId::new("a1"),
                },
            }),
        });
        self.event(KernelEvent::MessageDone {
            message: Message::Assistant(AssistantMessage {
                id: MessageId::new("a2"),
                content: vec![ContentBlock::Text {
                    text: "The run is green.".into(),
                }],
                model: "scripted".into(),
                stop_reason: None,
                usage: None,
            }),
        });
    }

    fn draw(&mut self) {
        let now = self.app.started + Duration::from_secs(30);
        let frame = crate::ui::draw::draw(&mut self.app, &mut self.cache, now, 0, 100, 40);
        self.lines = frame
            .lines
            .iter()
            .map(rust_cli_tui_kit::render::StyledLine::plain)
            .collect();
    }

    /// `needle` 第一次出现处往右 2 格的（列，行）。
    fn at(&self, needle: &str) -> (u16, u16) {
        let (y, line) = self
            .lines
            .iter()
            .enumerate()
            .find(|(_, line)| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} in {:#?}", self.lines));
        let x = string_width(&line[..line.find(needle).unwrap()]) + 2;
        (x as u16, y as u16)
    }

    /// 鼠标移到 `(x, y)`。
    fn hover(&mut self, (x, y): (u16, u16)) {
        let _ = self.app.on_mouse(
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            },
            self.app.started + Duration::from_secs(31),
        );
    }

    fn click(&mut self, (x, y): (u16, u16), modifiers: KeyModifiers) -> Option<Action> {
        let at = self.app.started + Duration::from_secs(31);
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            let action = self.app.on_mouse(
                MouseEvent {
                    kind,
                    column: x,
                    row: y,
                    modifiers,
                },
                at,
            );
            if matches!(kind, MouseEventKind::Up(_)) {
                return action;
            }
        }
        None
    }
}

fn opens(url: &str) -> Option<Action> {
    Some(Action::OpenLink(url.to_owned()))
}

/// 你的消息与工具的输出（^O 看得见输出）里的网址单击打开。
#[test]
fn urls_in_your_message_and_in_tool_output_open_on_a_click() {
    let mut s = Screen::new(true);
    s.turn();
    s.app.details_open = true;
    s.draw();
    let asked = s.at(ASKED);
    // 悬停：同回答里的链接（§9.1）。
    s.hover(asked);
    assert_eq!(s.app.hovered_link().map(|(url, _)| url), Some(ASKED.into()));
    assert_eq!(s.click(asked, KeyModifiers::NONE), opens(ASKED));
    let printed = s.at(PRINTED);
    assert_eq!(s.click(printed, KeyModifiers::NONE), opens(PRINTED));
    // 网址旁边的字（JSON 的键）不是链接。
    let key = s.at("\"run\"");
    assert_ne!(s.click(key, KeyModifiers::NONE), opens(PRINTED));
}

/// 输入框上方那一句（本地命令的结果）里的网址单击打开；按住修饰键时不开（终端自己开）；没有打开方式时不开。
#[test]
fn a_url_in_the_line_above_the_input_opens_on_a_click() {
    let mut s = Screen::new(true);
    s.app.info(format!("Docs: {SAID}"));
    s.draw();
    let at = s.at(SAID);
    // 悬停铺在这个网址的格上（可点击区域）。
    s.hover(at);
    let lit = s.app.hovered_region().expect("the url is a region");
    assert_eq!(usize::from(lit.width), string_width(SAID));
    assert_eq!(s.click(at, KeyModifiers::CONTROL), None);
    assert_eq!(s.click(at, KeyModifiers::NONE), opens(SAID));
    let mut closed = Screen::new(false);
    closed.app.info(format!("Docs: {SAID}"));
    closed.draw();
    let at = closed.at(SAID);
    assert_eq!(closed.click(at, KeyModifiers::NONE), None);
}
