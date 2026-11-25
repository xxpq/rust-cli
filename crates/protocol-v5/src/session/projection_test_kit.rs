//! 投影测试的共用部件（工具行与续写的测试用；`projection_tests.rs` 有它自己的一份）：空视图、来源、带窗口的 Publisher、逐个事件发布。

use rust_cli_kernel_api::ids::{SessionId, ToolUseId, TraceId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::metering::ToolTime;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    InputEnd, KernelEvent, SequencedEvent, SessionView, TurnOrigin, TurnTime,
};
use rust_cli_kernel_api::session_facts::TitleSource;
use rust_cli_kernel_api::store::{SessionKind, SessionMeta, TurnRecord};
use serde_json::{Value, json};

use super::*;
use crate::consts::{SESSION_SNAPSHOT_BYTES, SESSION_SNAPSHOT_ROWS};

/// 长会话视图里每个回合的工具调用数。
pub(crate) const TOOLS_PER_TURN: usize = 5;

/// 回合结束的时刻与用时（protocol-v5.md §29.1）：活会话的 `TurnFinished` 与冷读的回合记录用同一份，两边才是同一组行。
pub(super) const ENDED: TurnTime = TurnTime {
    ended_at: 9_000,
    active_ms: 4_000,
};

/// 回合结束时再写的那一份结局：带 [`ENDED`]，与用它的活会话 `TurnFinished` 同值。
pub(crate) fn ended(
    outcome: rust_cli_kernel_api::session_api::InputEnd,
    cancelled: Vec<rust_cli_kernel_api::ids::ToolUseId>,
) -> Option<rust_cli_kernel_api::store::TurnEnded> {
    Some(rust_cli_kernel_api::store::TurnEnded {
        outcome,
        cancelled,
        time: Some(ENDED),
        files: Vec::new(),
    })
}

/// 一条回合记录：从 `first` 这条消息开始、还没有结局（各测试按需改字段）。
pub(crate) fn turn_record(
    turn: &str,
    first: &str,
    origin: rust_cli_kernel_api::session_api::TurnOrigin,
) -> TurnRecord {
    TurnRecord {
        turn: rust_cli_kernel_api::ids::TurnId::new(turn),
        first: rust_cli_kernel_api::ids::MessageId::new(first),
        origin,
        started_at: 100,
        command: None,
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    }
}

pub(crate) fn view(messages: Vec<Message>, turns: Vec<TurnRecord>) -> SessionView {
    let meta = SessionMeta {
        id: SessionId::new("ses_1"),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: rust_cli_kernel_api::env::WorkspaceRef {
            environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
            root: rust_cli_kernel_api::env::EnvPath::new("/Users/dev/a"),
        },
        title: String::new(),
        trace: TraceId::new("ses_1"),
        created_at_ms: 1,
    };
    SessionView {
        turns,
        ..SessionView::new(
            meta,
            messages,
            rust_cli_kernel_api::testing::selection(),
            PermissionMode::Default,
        )
    }
}

pub(crate) fn origin(live: bool) -> Origin {
    Origin {
        live,
        draft: false,
        deleted: false,
        title_source: TitleSource::Default,
        mcp: Value::Null,
        subagents: Vec::new(),
        stamp: None,
        start: super::Start::Whole,
    }
}

pub(crate) fn publisher() -> Publisher {
    Publisher::new("ws/a/session/ses_1", "e")
        .with_window(
            rows::ROWS,
            rows::CREATED_AT_SEQ,
            SESSION_SNAPSHOT_ROWS,
            SESSION_SNAPSHOT_BYTES,
        )
        .with_openings(rows::opens)
}

/// 活会话：空视图起步。
pub(super) fn live() -> (Projection, Publisher) {
    let (projection, content) = Projection::build(&view(Vec::new(), Vec::new()), origin(true));
    let mut publisher = publisher();
    publisher.adopt(content);
    (projection, publisher)
}

/// 逐个事件发布（seq 接着投影已应用的往下编）。
pub(super) fn apply(
    publisher: &mut Publisher,
    projection: &mut Projection,
    events: Vec<KernelEvent>,
) {
    for event in events {
        let seq = projection.applied + 1;
        let batch = projection.apply(
            publisher,
            &SequencedEvent {
                seq,
                event,
                revision: 0,
            },
        );
        publisher.publish(batch);
    }
}

pub(crate) fn user(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

pub(crate) fn assistant(id: &str, content: Vec<ContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content,
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        stop_reason: None,
        usage: None,
    })
}

/// `turns` 个回合的冷视图：每个回合一条输入、[`TOOLS_PER_TURN`] 次调用与结果（各 `output_bytes` 字节）、一段回答；回合记录与工具的起止都齐。
/// 回合 `t` 的行：回合头 `turn_{t}`、输入 `msg_u{t}`、调用 `toolu_{t}_{k}`、回答 `msg_t{t}`（MEM-V5 的规模与驻留测试共用）。
pub(crate) fn long_view(turns: usize, output_bytes: usize) -> SessionView {
    let output = "x".repeat(output_bytes);
    let mut messages = Vec::new();
    let mut records = Vec::new();
    let mut times = Vec::new();
    for t in 0..turns {
        let prompt = format!("msg_u{t}");
        messages.push(user(&prompt, "read the files"));
        for k in 0..TOOLS_PER_TURN {
            let call = format!("toolu_{t}_{k}");
            let asked = format!("msg_a{t}_{k}");
            messages.push(assistant(
                &asked,
                vec![ContentBlock::ToolUse {
                    id: ToolUseId::new(&call),
                    name: rust_cli_kernel_api::tool_names::READ.to_owned(),
                    input: json!({"file_path": format!("/Users/dev/a/f{t}_{k}.txt")}),
                }],
            ));
            messages.push(Message::User(UserMessage {
                id: MessageId::new(format!("msg_r{t}_{k}")),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: ToolUseId::new(&call),
                    content: vec![ToolResultContent::Text {
                        text: output.clone(),
                    }],
                    is_error: false,
                }],
                origin: UserOrigin::ToolResult {
                    assistant: MessageId::new(&asked),
                },
            }));
            times.push(ToolTime {
                tool_use: ToolUseId::new(&call),
                started_at: 100,
                ended_at: 200,
            });
        }
        messages.push(assistant(
            &format!("msg_t{t}"),
            vec![ContentBlock::Text {
                text: "done reading".to_owned(),
            }],
        ));
        let mut record = turn_record(&format!("turn_{t}"), &prompt, TurnOrigin::Input);
        record.ended = ended(InputEnd::Completed, Vec::new());
        records.push(record);
    }
    SessionView {
        tool_times: times,
        ..view(messages, records)
    }
}
