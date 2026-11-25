//! 回合的页脚（docs/specs/rust-cli/tui-kit.md §4.0「尾巴」，2026-11-13）：回合在跑时尾巴最后总是 `✻ Cooking for 2m 5s`，
//! 不论笔骑在回答末尾、在工作段里还是单独一行；等模型的点阵只有 `⋮⋮⋮`，不再写 `Working`。
//!
//! 修复前（2026-11-12 的 `Working  2m 05s` 一行）：点阵在工作段等模型时，工作段的 `└ ⋮⋮⋮ Working` 与尾巴的
//! `Working  2m 05s` 成了两行 `Working`。
use std::time::Duration;

use rust_cli_kernel_api::ids::{MessageId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::BlockDelta;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, TurnOrigin};
use rust_cli_kernel_api::tool::ToolOutput;
use serde_json::json;

use super::{Cache, lines};
use crate::ui::app::App;
use crate::ui::cells::Ctx;
use crate::ui::i18n::Lang;
use crate::ui::test_support::app_in;

/// 回合 `t1` 挑的动词（结束那一行是 `Crunched`）。
const VERB_ING: &str = "Crunching";

/// 一次喂进一个新视图（seq 从 1 起），回合在 1 000 ms 开始。
fn feed(app: &mut App, events: Vec<KernelEvent>) {
    for (index, event) in events.into_iter().enumerate() {
        let seq = index as u64 + 1;
        app.view.apply(
            SequencedEvent {
                seq,
                revision: 0,
                event,
            },
            1_000,
        );
    }
}

fn started() -> KernelEvent {
    KernelEvent::TurnStarted {
        turn: TurnId::new("t1"),
        command: None,
        origin: TurnOrigin::Input,
        started_at: 0,
        works: Vec::new(),
    }
}

/// 回合在 1 000 ms 开始；`text` 不为空时回答在流式输出（笔骑在它的末尾）。
fn running(lang: Lang, text: &str) -> App {
    let mut app = app_in(lang);
    let mut events = vec![started()];
    if !text.is_empty() {
        events.push(KernelEvent::StreamDelta {
            turn: TurnId::new("t1"),
            response: 0,
            message: MessageId::new("msg_1"),
            index: 0,
            tool_use: None,
            delta: BlockDelta::Text(text.into()),
        });
    }
    feed(&mut app, events);
    app
}

/// 回合跑完一次 Bash，在等模型（工作段此刻的那一行是等模型的笔）。
fn after_a_tool() -> App {
    let mut app = app_in(Lang::En);
    let tool = ToolUseId::new("tu1");
    feed(
        &mut app,
        vec![
            started(),
            KernelEvent::ToolStarted {
                tool_use: tool.clone(),
                name: "Bash".into(),
                input: json!({"command": "cargo test"}),
                started_at: 0,
            },
            KernelEvent::MessageDone {
                message: Message::Assistant(AssistantMessage {
                    id: MessageId::new("a1"),
                    content: vec![ContentBlock::ToolUse {
                        id: tool.clone(),
                        name: "Bash".into(),
                        input: json!({"command": "cargo test"}),
                    }],
                    model: "scripted".into(),
                    stop_reason: None,
                    usage: None,
                }),
            },
            KernelEvent::ToolDone {
                tool_use: tool.clone(),
                output: ToolOutput {
                    content: vec![ToolResultContent::Text { text: "ok".into() }],
                    is_error: false,
                    display: serde_json::Value::Null,
                },
                cancelled: false,
            },
            KernelEvent::MessageDone {
                message: Message::User(UserMessage {
                    id: MessageId::new("r1"),
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: tool,
                        content: vec![ToolResultContent::Text { text: "ok".into() }],
                        is_error: false,
                    }],
                    origin: UserOrigin::ToolResult {
                        assistant: MessageId::new("a1"),
                    },
                }),
            },
        ],
    );
    app
}

/// 在 `now_ms`、动画走到 `elapsed` 时画出的 transcript，去掉行尾的空白。
fn screen(app: &App, cache: &mut Cache, now_ms: u64, elapsed: Duration) -> Vec<String> {
    let ctx = Ctx {
        ink: &app.ink,
        lang: app.lang,
        width: 80,
        expanded: false,
        elapsed,
        now_ms,
        workspace: "/Users/dev/project",
        workspace_label: "~/project",
        version: "0.1.0",
        mcp_machines: &[],
        machines: &[],
        skills: &[],
        markdown: None,
    };
    let layout = lines(app, cache, &ctx);
    (0..layout.len())
        .map(|index| layout.styled(index).plain().trim_end().to_owned())
        .collect()
}

/// 最后一行不是空行的那一行，与它上面那一行。
fn last_two(app: &App, now_ms: u64) -> (String, String) {
    let shown = screen(app, &mut Cache::default(), now_ms, Duration::from_secs(5));
    let mut filled = shown.iter().rposition(|line| !line.is_empty()).unwrap();
    let last = shown[filled].clone();
    filled = filled.saturating_sub(1);
    (shown[filled].clone(), last)
}

#[test]
fn the_footer_counts_the_turn_while_the_answer_streams() {
    let app = running(Lang::En, "Now writing the analysis");
    let (above, last) = last_two(&app, 126_000);
    assert_eq!(last, format!(" ✻ {VERB_ING} for 2m 5s"));
    assert!(above.is_empty(), "a blank line above the footer: {above:?}");
}

#[test]
fn the_footer_is_under_the_lone_cursor_and_in_chinese() {
    let app = running(Lang::Zh, "");
    let (_, last) = last_two(&app, 8_000);
    assert_eq!(last, " ✻ 用时 7s");
}

#[test]
fn under_a_second_the_footer_says_only_the_verb() {
    let app = running(Lang::En, "Now writing");
    let (_, last) = last_two(&app, 1_500);
    assert_eq!(last, format!(" ✻ {VERB_ING}"));
}

/// 用户报告的样子：跑完工具等模型、手停住半秒之后，工作段是 `└ ⋮⋮⋮`；屏幕上没有 `Working`，用时只在页脚。
#[test]
fn waiting_for_the_model_after_a_tool_shows_only_the_dots() {
    let app = after_a_tool();
    let mut cache = Cache::default();
    screen(&app, &mut cache, 60_000, Duration::from_secs(5));
    let shown = screen(&app, &mut cache, 61_000, Duration::from_secs(6));
    assert!(
        !shown.iter().any(|line| line.contains("Working")),
        "{shown:#?}"
    );
    assert!(
        shown.iter().any(|line| line.trim_end().ends_with("└ ⋮⋮⋮")),
        "{shown:#?}"
    );
    assert_eq!(
        shown.iter().rfind(|line| !line.is_empty()).unwrap(),
        &format!(" ✻ {VERB_ING} for 1m 0s")
    );
}
