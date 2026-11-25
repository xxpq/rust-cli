//! MCP 服务器的询问在 TUI 里（tui-kit.md §4.3「MCP 的询问」）：表单一个字段一页、最后一页提交或拒绝；打的字与提交时按 kernel-api
//! 的校验停在那一页说原因；esc 只取消这一问；网址同意之后打开；sampling 的三个回答。
use std::time::{Duration, Instant};

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::{InteractionId, ToolUseId};
use rust_cli_kernel_api::mcp_ask::{
    Choice, Elicitation, ElicitationKind, FieldKind, FormField, Sampling, SamplingMessage,
    SamplingRole,
};
use rust_cli_kernel_api::orchestration::{Answer, InteractionRequest};
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_tui_kit::app::widgets::asks::ARM_TICKS;
use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;
use serde_json::{Value, json};

use super::*;
use crate::ui::app::{Action, App};

const ARMED: Duration = Duration::from_millis(ARM_TICKS as u64 * TICK_MS + 1);
const URL: &str = "https://mcp.example.com/login";

fn text(name: &str, required: bool, min_length: Option<u32>) -> FormField {
    FormField {
        name: name.to_owned(),
        title: None,
        description: None,
        required,
        kind: FieldKind::Text {
            format: None,
            min_length,
            max_length: None,
            default: None,
        },
    }
}

fn boolean(name: &str, default: Option<bool>) -> FormField {
    FormField {
        name: name.to_owned(),
        title: Some("Subscribe".to_owned()),
        description: None,
        required: false,
        kind: FieldKind::Boolean { default },
    }
}

fn elicitation(kind: ElicitationKind) -> InteractionRequest {
    InteractionRequest::McpElicitation {
        tool_use: ToolUseId::new("tu1"),
        elicitation: Box::new(Elicitation {
            server: "srv".to_owned(),
            message: "Who are you?".to_owned(),
            kind,
        }),
        asked_by: None,
    }
}

fn form(fields: Vec<FormField>) -> InteractionRequest {
    elicitation(ElicitationKind::Form { fields })
}

fn open(request: InteractionRequest) -> (App, Instant) {
    let start = Instant::now();
    let mut app = crate::ui::test_support::app_in(Lang::En);
    app.view.apply(
        SequencedEvent {
            seq: 1,
            revision: 0,
            event: KernelEvent::InteractionAsked {
                interaction: InteractionId::new("i1"),
                request,
            },
        },
        0,
    );
    app.sync_interaction(start);
    (app, start + ARMED)
}

fn press(app: &mut App, keys: &[KeyCode], at: Instant) -> Option<Action> {
    keys.iter()
        .find_map(|code| app.on_key(crate::ui::test_support::key(*code), at))
}

fn chars(text: &str) -> Vec<KeyCode> {
    text.chars().map(KeyCode::Char).collect()
}

fn resolve(answer: Value) -> Action {
    Action::Resolve {
        interaction: InteractionId::new("i1"),
        answer: serde_json::from_value::<Answer>(answer).expect("an answer"),
    }
}

/// 页：字段一页（必填的页名带 `*`，布尔是「是 / 否」），最后一页提交或拒绝；网址一页。
#[test]
fn pages_follow_the_fields() {
    let InteractionRequest::McpElicitation {
        elicitation: asked, ..
    } = form(vec![text("name", true, None), boolean("news", None)])
    else {
        unreachable!();
    };
    let shown = pages(Lang::En, &asked);
    let headers: Vec<&str> = shown.iter().map(|page| page.header.as_str()).collect();
    assert_eq!(headers, ["name *", "Subscribe", "Submit"]);
    assert!(shown[0].options.is_empty());
    assert_eq!(
        shown[1]
            .options
            .iter()
            .map(|o| o.label.as_str())
            .collect::<Vec<_>>(),
        ["Yes", "No"]
    );
    assert!(
        shown[0]
            .question
            .starts_with("MCP server “srv” requests your input\n\nWho are you?")
    );
    let InteractionRequest::McpElicitation {
        elicitation: asked, ..
    } = elicitation(ElicitationKind::Url {
        url: URL.to_owned(),
    })
    else {
        unreachable!();
    };
    let url = pages(Lang::En, &asked);
    assert_eq!(url.len(), 1);
    assert!(url[0].question.ends_with(URL));
}

/// 填表：不合格的字停在那一页说原因；改好之后逐页走到最后，提交的是服务器要的类型。
#[test]
fn a_form_is_filled_page_by_page() {
    let (mut app, at) = open(form(vec![
        text("name", true, Some(2)),
        boolean("news", None),
    ]));
    assert_eq!(press(&mut app, &chars("a"), at), None);
    assert_eq!(press(&mut app, &[KeyCode::Enter], at), None);
    assert_eq!(
        app.said.failed.as_deref(),
        Some("Must be at least 2 characters")
    );
    assert_eq!(app.interaction.as_ref().map(|ui| ui.page), Some(0));
    assert_eq!(press(&mut app, &chars("b"), at), None);
    assert_eq!(press(&mut app, &[KeyCode::Enter], at), None);
    assert_eq!(app.interaction.as_ref().map(|ui| ui.page), Some(1));
    // 布尔选「是」，最后一页选「提交」
    let sent = press(&mut app, &[KeyCode::Char('1'), KeyCode::Char('1')], at);
    assert_eq!(
        sent,
        Some(resolve(
            json!({"decision": "accept", "content": {"name": "ab", "news": true}})
        ))
    );
}

/// 提交时整份校验：必填的没填，回到那一页说原因；拒绝不校验。
#[test]
fn submit_checks_the_whole_form_and_decline_does_not() {
    let (mut app, at) = open(form(vec![
        text("name", true, None),
        boolean("news", Some(false)),
    ]));
    // 跳过第一页（空的 ⏎），第二页的初值是「否」（schema 的 default），⏎ 选它
    assert_eq!(press(&mut app, &[KeyCode::Enter, KeyCode::Enter], at), None);
    assert_eq!(press(&mut app, &[KeyCode::Char('1')], at), None);
    assert_eq!(app.said.failed.as_deref(), Some("This field is required"));
    assert_eq!(app.interaction.as_ref().map(|ui| ui.page), Some(0));
    let (mut app, at) = open(form(vec![text("name", true, None)]));
    let sent = press(&mut app, &[KeyCode::Enter, KeyCode::Char('2')], at);
    assert_eq!(sent, Some(resolve(json!({"decision": "decline"}))));
}

/// esc 只取消这一问（不停回合）；槽里写 `esc cancel`。
#[test]
fn esc_cancels_the_elicitation_only() {
    let (mut app, at) = open(form(vec![text("name", false, None)]));
    let slot: Vec<String> = app
        .ask_slot(false)
        .into_iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect();
    assert!(slot.contains(&"esc cancel".to_owned()), "{slot:?}");
    let sent = press(&mut app, &[KeyCode::Esc], at);
    assert_eq!(sent, Some(resolve(json!({"decision": "cancel"}))));
    assert!(!app.asks.stopping);
}

/// 网址：同意时先回答，再打开网址。
#[test]
fn a_url_is_opened_after_accepting() {
    let (mut app, at) = open(elicitation(ElicitationKind::Url {
        url: URL.to_owned(),
    }));
    let sent = press(&mut app, &[KeyCode::Char('1')], at);
    assert_eq!(
        sent,
        Some(Action::Many(vec![
            resolve(json!({"decision": "accept"})),
            Action::OpenLink(URL.to_owned()),
        ]))
    );
}

/// sampling：允许一次、本会话一直允许、拒绝；板头是服务器借用模型。
#[test]
fn sampling_offers_three_answers() {
    let request = || InteractionRequest::McpSampling {
        tool_use: ToolUseId::new("tu1"),
        sampling: Box::new(Sampling {
            server: "srv".to_owned(),
            messages: vec![SamplingMessage {
                role: SamplingRole::User,
                text: "Summarize".to_owned(),
            }],
            system: None,
            max_tokens: 100,
        }),
        asked_by: None,
    };
    let (mut app, at) = open(request());
    let rows = app.ask_rows(at);
    assert!(!rows.is_empty());
    assert_eq!(
        press(&mut app, &[KeyCode::Char('2')], at),
        Some(resolve(
            json!({"decision": "sampling_allowed", "session": true})
        ))
    );
    let (mut app, at) = open(request());
    assert_eq!(
        press(&mut app, &[KeyCode::Char('3')], at),
        Some(resolve(json!({"decision": "sampling_denied"})))
    );
}

/// 选项字段的初值：选中 schema 的 default。
#[test]
fn defaults_preselect_the_choices() {
    let InteractionRequest::McpElicitation {
        elicitation: asked, ..
    } = form(vec![FormField {
        name: "color".to_owned(),
        title: None,
        description: None,
        required: false,
        kind: FieldKind::Choice {
            options: vec![
                Choice {
                    value: "r".to_owned(),
                    label: "Red".to_owned(),
                },
                Choice {
                    value: "g".to_owned(),
                    label: "Green".to_owned(),
                },
            ],
            default: Some("g".to_owned()),
        },
    }])
    else {
        unreachable!();
    };
    let (picks, own) = initial(&asked);
    assert_eq!(picks.get(&0), Some(&vec![1]));
    assert!(own.is_empty());
}
