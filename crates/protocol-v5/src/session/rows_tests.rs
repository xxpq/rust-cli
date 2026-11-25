//! 一条消息给出的行（protocol-v5.md §15.3.3）：显示的思考块、正文接起来、工具调用在后；插话是一行 guided 的用户输入。

use rust_cli_kernel_api::hooks::queued_command;
use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{AssistantMessage, AttachmentMessage, ContentBlock, Message};
use serde_json::json;

use super::*;

#[test]
fn an_assistant_gives_thinking_then_one_text_then_its_calls() {
    let message = Message::Assistant(AssistantMessage {
        id: MessageId::new("msg_a"),
        content: vec![
            ContentBlock::Text {
                text: "a".to_owned(),
            },
            ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_1"),
                name: "Read".to_owned(),
                input: json!({}),
            },
            // 没有文字也没有签名的思考块不显示（同 node-store 落库时的筛选）
            ContentBlock::Thinking {
                thinking: String::new(),
                signature: None,
            },
            ContentBlock::RedactedThinking {
                data: "x".to_owned(),
            },
            ContentBlock::Text {
                text: "b".to_owned(),
            },
        ],
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        stop_reason: None,
        usage: None,
    });
    assert_eq!(
        parts(&message),
        vec![
            Part::Reasoning {
                id: "msg_a:reasoning:0".to_owned(),
                text: String::new(),
            },
            Part::Text {
                id: "msg_a".to_owned(),
                text: "ab".to_owned(),
                model: rust_cli_base::models::GLM_5_3.to_owned(),
            },
            Part::Tool {
                id: "toolu_1".to_owned(),
                name: "Read".to_owned(),
                input: json!({}),
            },
        ]
    );
}

#[test]
fn an_absorbed_guide_is_a_guided_input_and_a_notice_is_not_a_row() {
    let attachment = |mode: &str| {
        Message::Attachment(AttachmentMessage {
            id: MessageId::new("msg_g"),
            source: queued_command::SOURCE.to_owned(),
            text: "<system-reminder>…</system-reminder>".to_owned(),
            data: json!({queued_command::MODE_KEY: mode, queued_command::PROMPT_KEY: "also this"}),
        })
    };
    assert_eq!(
        parts(&attachment(queued_command::PROMPT_MODE)),
        vec![Part::User {
            id: "msg_g".to_owned(),
            text: "also this".to_owned(),
            guided: true,
        }]
    );
    assert_eq!(
        parts(&attachment(queued_command::TASK_NOTIFICATION)),
        Vec::new()
    );
}

#[test]
fn the_interrupt_note_is_not_an_input() {
    let note = Message::User(rust_cli_kernel_api::message::UserMessage {
        id: MessageId::new("msg_i"),
        content: vec![ContentBlock::Text {
            text: rust_cli_kernel_api::user_input::INTERRUPT_TEXTS[0].to_owned(),
        }],
        origin: rust_cli_kernel_api::message::UserOrigin::Human,
    });
    assert_eq!(parts(&note), Vec::new());
}
