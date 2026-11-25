//! 回合头的工作段（protocol-v5.md §26.2，照 v4）：同一个工具边界并入的插话共用第一条开的一段，下一个工具边界的插话再开一段；时刻取自回合头与
//! 插话消息的 id，活会话与冷读同值；没有插话的回合不写；回合中途建的投影没有回合开始，不开段。
//! 回合的结束（§29.1）：回合头写结束的时刻与耗时，最后一段收口；活会话取 `TurnFinished.time`，冷读取回合记录，同值；旧记录没有就不写。

use rust_cli_kernel_api::hooks::queued_command;
use rust_cli_kernel_api::ids::{CommandId, MessageId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, ToolResultContent, UserMessage,
    UserOrigin,
};
use rust_cli_kernel_api::session_api::{
    InputEnd, KernelEvent, SessionView, TurnOrigin, TurnOutcome, TurnTime,
};
use rust_cli_kernel_api::store::{TurnEnded, TurnRecord};
use serde_json::{Value, json};

use super::super::kit::{apply, live, origin, publisher, view};
use super::*;

/// 消息 id 里的创建毫秒（`msg_<36 进制毫秒>_…`）：`rs` 是 1000，`1jk` 是 2000，`2bc` 是 3000。
const G1: &str = "msg_rs_g1";
const G2: &str = "msg_rs_g2";
const G3: &str = "msg_1jk_g3";
const G4: &str = "msg_2bc_g4";

fn user(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn tool_use(id: &str, call: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolUse {
            id: ToolUseId::new(call),
            name: "Write".to_owned(),
            input: json!({}),
        }],
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn answer(id: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "done".to_owned(),
        }],
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn result(id: &str, owner: &str, call: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(call),
            content: vec![ToolResultContent::Text {
                text: "ok".to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new(owner),
        },
    })
}

/// 工具边界并入的插话（`queued_command` attachment 的 prompt 模式）。
fn steered(id: &str) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new(id),
        source: queued_command::SOURCE.to_owned(),
        text: id.to_owned(),
        data: json!({ queued_command::PROMPT_KEY: id, queued_command::MODE_KEY: queued_command::PROMPT_MODE }),
    })
}

/// 回合记录：回合结束时再写的那一份（结局与结束的时刻、用时）。
fn record(turn: &str, first: &str, started_at: u64, time: TurnTime) -> TurnRecord {
    TurnRecord {
        turn: TurnId::new(turn),
        first: MessageId::new(first),
        origin: TurnOrigin::Input,
        started_at,
        command: Some(CommandId::new("c1")),
        ended: Some(TurnEnded {
            outcome: InputEnd::Completed,
            cancelled: Vec::new(),
            time: Some(time),
            files: Vec::new(),
        }),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    }
}

/// 第一个回合在 2400 结束（扣掉等回答之后用了 1000），第二个在 3600 结束（用了 500）。
const TURN_1_ENDED: TurnTime = TurnTime {
    ended_at: 2_400,
    active_ms: 1_000,
};
const TURN_2_ENDED: TurnTime = TurnTime {
    ended_at: 3_600,
    active_ms: 500,
};

/// 两个回合：第一个回合里一个工具边界并入 g1、g2，下一个边界并入 g3；第二个回合一个边界并入 g4。
fn turns() -> Vec<(TurnRecord, Vec<Message>)> {
    vec![
        (
            record("turn_1", "msg_u1", 100, TURN_1_ENDED),
            vec![
                user("msg_u1", "base"),
                tool_use("msg_a1", "toolu_1"),
                result("msg_r1", "msg_a1", "toolu_1"),
                steered(G1),
                steered(G2),
                tool_use("msg_a2", "toolu_2"),
                result("msg_r2", "msg_a2", "toolu_2"),
                steered(G3),
                answer("msg_a3"),
            ],
        ),
        (
            record("turn_2", "msg_u2", 2500, TURN_2_ENDED),
            vec![
                user("msg_u2", "again"),
                tool_use("msg_a4", "toolu_3"),
                result("msg_r3", "msg_a4", "toolu_3"),
                steered(G4),
                answer("msg_a5"),
            ],
        ),
    ]
}

fn segments(publisher: &Publisher, turn: &str) -> Value {
    publisher
        .entity(rows::ROWS, turn)
        .unwrap()
        .get("workSegments")
        .cloned()
        .unwrap_or(Value::Null)
}

/// 活会话：两个回合逐个事件发布，`TurnFinished` 带的时刻与回合记录里的同值。
fn followed() -> Publisher {
    let (mut projection, mut publisher) = live();
    for (record, messages) in turns() {
        let mut events = vec![KernelEvent::TurnStarted {
            turn: record.turn.clone(),
            command: record.command.clone(),
            origin: record.origin,
            started_at: record.started_at,
            works: Vec::new(),
        }];
        events.extend(
            messages
                .into_iter()
                .map(|message| KernelEvent::MessageDone { message }),
        );
        let time = record.ended.as_ref().and_then(|ended| ended.time).unwrap();
        events.push(KernelEvent::TurnFinished {
            turn: record.turn,
            outcome: TurnOutcome::Success,
            usage: None,
            time,
            files: Vec::new(),
        });
        apply(&mut publisher, &mut projection, events);
    }
    publisher
}

/// 冷读：视图的消息与回合记录（`records` 改写之后）。
fn cold(records: impl Fn(TurnRecord) -> TurnRecord) -> Publisher {
    let (records, messages): (Vec<_>, Vec<_>) = turns()
        .into_iter()
        .map(|(record, messages)| (records(record), messages))
        .unzip();
    let (_, content) = Projection::build(
        &view(messages.into_iter().flatten().collect(), records),
        origin(false),
    );
    let mut cold = super::super::kit::publisher();
    cold.adopt(content);
    cold
}

/// 修 bug（M06f，protocol-v5.md §26.0）：以前回合头不写 `workSegments`，v5 读成 `[]`，界面不切段，插话之后的工作没有自己的「已工作」。
/// 修 bug（TURN-TIME，§29）：以前最后一段没有结束，界面显示不带时长的「已处理」；现在回合结束时按结束的时刻收口。
#[test]
fn guides_of_one_boundary_share_a_segment_live_and_cold() {
    let publisher = followed();
    assert_eq!(
        segments(&publisher, "turn_1"),
        json!([
            {"segmentId": "turn_1:initial", "startedAt": 100, "endedAt": 1000, "activeMs": 900},
            {"segmentId": G1, "triggerEntityId": G1, "startedAt": 1000, "endedAt": 2000, "activeMs": 1000},
            {"segmentId": G3, "triggerEntityId": G3, "startedAt": 2000, "endedAt": 2400, "activeMs": 400},
        ])
    );
    assert_eq!(
        segments(&publisher, "turn_2"),
        json!([
            {"segmentId": "turn_2:initial", "startedAt": 2500, "endedAt": 3000, "activeMs": 500},
            {"segmentId": G4, "triggerEntityId": G4, "startedAt": 3000, "endedAt": 3600, "activeMs": 600},
        ])
    );
    assert_eq!(
        publisher.entity(rows::ROWS, "turn_1").unwrap()["state"],
        json!(rows::turn_state::COMPLETED)
    );
    // 冷读（视图的消息与回合记录）：同一组段
    let cold = cold(|record| record);
    for turn in ["turn_1", "turn_2"] {
        assert_eq!(segments(&cold, turn), segments(&publisher, turn), "{turn}");
    }
}

/// 回合头的结束时刻与耗时（§29.1）：`activeMs` 是内核扣掉等回答之后的用时，`totalMs` 是结束减回合头的开始；活会话与冷读同值。
#[test]
fn the_turn_header_has_its_end_and_durations_live_and_cold() {
    let publisher = followed();
    let header = |publisher: &Publisher, turn: &str| {
        let row = publisher.entity(rows::ROWS, turn).unwrap();
        (row["endedAt"].clone(), row["durations"].clone())
    };
    assert_eq!(
        header(&publisher, "turn_1"),
        (
            rows::millis(2_400),
            json!({"activeMs": rows::millis(1_000), "totalMs": rows::millis(2_300)})
        )
    );
    assert_eq!(
        header(&publisher, "turn_2"),
        (
            rows::millis(3_600),
            json!({"activeMs": rows::millis(500), "totalMs": rows::millis(1_100)})
        )
    );
    let cold = cold(|record| record);
    for turn in ["turn_1", "turn_2"] {
        assert_eq!(header(&cold, turn), header(&publisher, turn), "{turn}");
    }
}

/// 中断与失败的回合也写（v4 的这两种也有结束时刻；v5 的 `durations` 缺省是 0，不写就显示「1 秒」）。
#[test]
fn stopped_and_failed_turns_also_have_their_end() {
    for outcome in [
        TurnOutcome::Interrupted,
        TurnOutcome::Failed {
            message: "boom".to_owned(),
            cause: None,
        },
    ] {
        let (mut projection, mut publisher) = live();
        apply(
            &mut publisher,
            &mut projection,
            vec![
                KernelEvent::TurnStarted {
                    turn: TurnId::new("turn_1"),
                    command: None,
                    origin: TurnOrigin::Input,
                    started_at: 100,
                    works: Vec::new(),
                },
                KernelEvent::MessageDone {
                    message: user("msg_u1", "base"),
                },
                KernelEvent::TurnFinished {
                    turn: TurnId::new("turn_1"),
                    outcome: outcome.clone(),
                    usage: None,
                    time: TURN_1_ENDED,
                    files: Vec::new(),
                },
            ],
        );
        let row = publisher.entity(rows::ROWS, "turn_1").unwrap();
        assert_eq!(row["endedAt"], rows::millis(2_400), "{outcome:?}");
        assert_eq!(
            row["durations"],
            json!({"activeMs": rows::millis(1_000), "totalMs": rows::millis(2_300)}),
            "{outcome:?}"
        );
    }
}

/// 本功能之前写的回合记录没有结束的时刻：回合头不写结束与耗时（读成契约的缺省），最后一段照旧开着。不补。
#[test]
fn an_old_record_without_its_end_leaves_the_header_and_the_last_segment_open() {
    let cold = cold(|mut record| {
        if let Some(ended) = record.ended.as_mut() {
            ended.time = None;
        }
        record
    });
    let row = cold.entity(rows::ROWS, "turn_1").unwrap();
    assert!(row.get("endedAt").is_none(), "{row:?}");
    assert!(row.get("durations").is_none(), "{row:?}");
    assert_eq!(
        segments(&cold, "turn_1")[2],
        json!({"segmentId": G3, "triggerEntityId": G3, "startedAt": 2000})
    );
}

/// 没有插话的回合不写 `workSegments`（v4 同：普通回合缺省）。
#[test]
fn a_turn_without_guides_has_no_segments() {
    let (mut projection, mut publisher) = live();
    apply(
        &mut publisher,
        &mut projection,
        vec![
            KernelEvent::TurnStarted {
                turn: TurnId::new("turn_1"),
                command: None,
                origin: TurnOrigin::Input,
                started_at: 100,
                works: Vec::new(),
            },
            KernelEvent::MessageDone {
                message: user("msg_u1", "base"),
            },
            KernelEvent::MessageDone {
                message: answer("msg_a1"),
            },
        ],
    );
    assert_eq!(segments(&publisher, "turn_1"), Value::Null);
}

/// 回合中途建的投影：视图带着在跑回合的开始（`SessionView.running` 的 `Running::Turn`，session-lifecycle.md §10.35），所以之后并入的插话照常开段，
/// 与从回合开始就跟着的活会话开出同样的段。以前视图只给回合 id，中途建的投影没有开始时刻、不开段（PROJ-GAPS 写这条时的情形）。
#[test]
fn a_projection_built_mid_turn_opens_the_same_segments_as_a_live_one() {
    let start = rust_cli_kernel_api::session_api::TurnStart {
        turn: TurnId::new("turn_1"),
        command: None,
        origin: TurnOrigin::Input,
        started_at: 100,
        works: Vec::new(),
    };
    let rest = || {
        vec![
            KernelEvent::MessageDone {
                message: tool_use("msg_a1", "toolu_1"),
            },
            KernelEvent::MessageDone {
                message: result("msg_r1", "msg_a1", "toolu_1"),
            },
            KernelEvent::MessageDone {
                message: steered(G1),
            },
        ]
    };
    // 从回合开始就跟着
    let (mut followed, content) = Projection::build(&view(Vec::new(), Vec::new()), origin(true));
    let mut live_publisher = publisher();
    live_publisher.adopt(content);
    let mut events = vec![KernelEvent::TurnStarted {
        turn: start.turn.clone(),
        command: start.command.clone(),
        origin: start.origin,
        started_at: start.started_at,
        works: Vec::new(),
    }];
    events.extend(rest());
    apply(&mut live_publisher, &mut followed, events);
    // 回合中途由视图建
    let running = SessionView {
        running: Some(rust_cli_kernel_api::session_api::Running::Turn(start)),
        ..view(Vec::new(), Vec::new())
    };
    let (mut projection, content) = Projection::build(&running, origin(true));
    let mut publisher = publisher();
    publisher.adopt(content);
    apply(&mut publisher, &mut projection, rest());
    let opened = segments(&live_publisher, "turn_1");
    assert_ne!(opened, Value::Null, "活会话应当开了段");
    assert_eq!(segments(&publisher, "turn_1"), opened);
}
