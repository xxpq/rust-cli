//! 输出上限之后的续写拼进同一行（protocol-v5.md §19.9，13 Q16）：活会话与冷读同一行；三段拼成一行；续写先出思考时各自成行；
//! 失败、重发时放回续写之前的正文；不是续写提示的 meta 不拼。

use rust_cli_kernel_api::ids::{CommandId, MessageId, TurnId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, OUTPUT_LIMIT_RESUME_SOURCE, StopReason, UserMessage,
    UserOrigin,
};
use rust_cli_kernel_api::model::BlockDelta;
use rust_cli_kernel_api::session_api::{KernelEvent, TurnOrigin, TurnOutcome};
use rust_cli_kernel_api::store::TurnRecord;
use serde_json::{Value, json};

use super::super::kit::{apply, live, origin, view};
use super::*;

const TURN: &str = "turn_1";

fn user() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_u"),
        content: vec![ContentBlock::Text {
            text: "write a long answer".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn nudge(id: &str, source: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "Output token limit hit.".to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: source.to_owned(),
        },
    })
}

fn answer(id: &str, content: Vec<ContentBlock>, stop: StopReason) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content,
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        stop_reason: Some(stop),
        usage: None,
    })
}

fn text(id: &str, text: &str, stop: StopReason) -> Message {
    answer(
        id,
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        stop,
    )
}

fn delta(message: &str, delta: BlockDelta) -> KernelEvent {
    KernelEvent::StreamDelta {
        turn: TurnId::new(TURN),
        response: 0,
        message: MessageId::new(message),
        index: 0,
        tool_use: None,
        delta,
    }
}

fn done(message: Message) -> KernelEvent {
    KernelEvent::MessageDone { message }
}

fn started() -> KernelEvent {
    KernelEvent::TurnStarted {
        turn: TurnId::new(TURN),
        command: Some(CommandId::new("c1")),
        origin: TurnOrigin::Input,
        started_at: 100,
        works: Vec::new(),
    }
}

fn finished(outcome: TurnOutcome) -> KernelEvent {
    KernelEvent::TurnFinished {
        turn: TurnId::new(TURN),
        outcome,
        usage: None,
        time: super::super::kit::ENDED,
        files: Vec::new(),
    }
}

fn record() -> TurnRecord {
    TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new("msg_u"),
        origin: TurnOrigin::Input,
        started_at: 100,
        command: Some(CommandId::new("c1")),
        // 回合结束时再写的那一份：与活会话的 `finished(Success)` 同一个结局与时刻
        ended: super::super::kit::ended(
            rust_cli_kernel_api::session_api::InputEnd::Completed,
            Vec::new(),
        ),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    }
}

/// 全部行（快照里的），按 createdAtSeq，去掉版本。
fn rows_of(publisher: &Publisher) -> Vec<Value> {
    let frame = publisher.frame_for(None, "s", false);
    let crate::generated::core_v1::FrameBody::Snapshot(snapshot) = frame.body else {
        panic!("not a snapshot")
    };
    let value = serde_json::to_value(snapshot.snapshot).unwrap();
    let mut rows: Vec<Value> = value["collections"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let mut row = row.clone();
            row.as_object_mut().unwrap().remove("v");
            row
        })
        .collect();
    rows.sort_by_key(|row| row[rows::CREATED_AT_SEQ].as_u64());
    rows
}

fn assistant_texts(rows: &[Value]) -> Vec<(String, String)> {
    rows.iter()
        .filter(|row| row["kind"] == json!("assistantText"))
        .map(|row| {
            (
                row["id"].as_str().unwrap_or_default().to_owned(),
                row["text"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// 冷读：视图里的消息（全部是这个回合的）。
fn cold(messages: Vec<Message>) -> Vec<Value> {
    let (_, content) = Projection::build(&view(messages, vec![record()]), origin(false));
    let mut publisher = super::super::kit::publisher();
    publisher.adopt(content);
    rows_of(&publisher)
}

/// 修 bug（19-accept.md §15.4 R-OUTPUT-CONTINUE，13 Q16）：以前续写另起一行，跨断点的段落、表格、代码块分成两段渲染。
/// 现在接进上一条的正文行（照 v4），活会话与冷读是同一行。
#[test]
fn a_continuation_joins_the_previous_row_live_and_cold() {
    let a = text("msg_a", "Hello wor", StopReason::MaxTokens);
    let m = nudge("msg_m", OUTPUT_LIMIT_RESUME_SOURCE);
    let b = text("msg_b", "ld!", StopReason::EndTurn);
    let (mut projection, mut publisher) = live();
    apply(
        &mut publisher,
        &mut projection,
        vec![
            started(),
            done(user()),
            delta("msg_a", BlockDelta::Text("Hello wor".to_owned())),
            done(a.clone()),
            done(m.clone()),
            delta("msg_b", BlockDelta::Text("ld".to_owned())),
            delta("msg_b", BlockDelta::Text("!".to_owned())),
        ],
    );
    // 流式时已经接上
    assert_eq!(
        assistant_texts(&rows_of(&publisher)),
        vec![("msg_a".to_owned(), "Hello world!".to_owned())]
    );
    apply(
        &mut publisher,
        &mut projection,
        vec![done(b.clone()), finished(TurnOutcome::Success)],
    );
    let live_rows = rows_of(&publisher);
    assert_eq!(
        assistant_texts(&live_rows),
        vec![("msg_a".to_owned(), "Hello world!".to_owned())]
    );
    let row = live_rows
        .iter()
        .find(|row| row["id"] == json!("msg_a"))
        .unwrap();
    assert_eq!(row["state"], json!("done"));
    assert_eq!(row["assistantResponseId"], json!("msg_b"));
    assert_eq!(live_rows, cold(vec![user(), a, m, b]));
}

/// 最多续写 3 次：三段拼成一行。
#[test]
fn every_continuation_of_a_turn_joins_the_same_row() {
    let messages = vec![
        user(),
        text("msg_a", "one ", StopReason::MaxTokens),
        nudge("msg_m1", OUTPUT_LIMIT_RESUME_SOURCE),
        text("msg_b", "two ", StopReason::MaxTokens),
        nudge("msg_m2", OUTPUT_LIMIT_RESUME_SOURCE),
        text("msg_c", "three", StopReason::EndTurn),
    ];
    let (mut projection, mut publisher) = live();
    let mut events = vec![started()];
    events.extend(messages.iter().cloned().map(done));
    events.push(finished(TurnOutcome::Success));
    apply(&mut publisher, &mut projection, events);
    let live_rows = rows_of(&publisher);
    assert_eq!(
        assistant_texts(&live_rows),
        vec![("msg_a".to_owned(), "one two three".to_owned())]
    );
    assert_eq!(live_rows, cold(messages));
}

/// 续写先出了思考（v4：思考行先成了最后一行，不再拼）：续写有自己的行，前一行不动。
#[test]
fn a_continuation_that_starts_with_thinking_keeps_its_own_rows() {
    let b = answer(
        "msg_b",
        vec![
            ContentBlock::Thinking {
                thinking: "hmm".to_owned(),
                signature: Some("sig".to_owned()),
            },
            ContentBlock::Text {
                text: "ld!".to_owned(),
            },
        ],
        StopReason::EndTurn,
    );
    let messages = vec![
        user(),
        text("msg_a", "Hello wor", StopReason::MaxTokens),
        nudge("msg_m", OUTPUT_LIMIT_RESUME_SOURCE),
        b,
    ];
    let (mut projection, mut publisher) = live();
    let mut events = vec![started()];
    events.extend(messages.iter().cloned().map(done));
    events.push(finished(TurnOutcome::Success));
    apply(&mut publisher, &mut projection, events);
    assert_eq!(
        assistant_texts(&rows_of(&publisher)),
        vec![
            ("msg_a".to_owned(), "Hello wor".to_owned()),
            ("msg_b".to_owned(), "ld!".to_owned())
        ]
    );
    assert_eq!(rows_of(&publisher), cold(messages));
}

/// 续写的请求失败（回合以出错结束，没有 MessageDone）：前一行放回续写之前的正文，与冷读（没有续写的消息）相同。
#[test]
fn a_failed_continuation_puts_the_row_back() {
    let a = text("msg_a", "Hello wor", StopReason::MaxTokens);
    let m = nudge("msg_m", OUTPUT_LIMIT_RESUME_SOURCE);
    let (mut projection, mut publisher) = live();
    apply(
        &mut publisher,
        &mut projection,
        vec![
            started(),
            done(user()),
            done(a),
            done(m),
            delta("msg_b", BlockDelta::Text("ld".to_owned())),
            finished(TurnOutcome::Interrupted),
        ],
    );
    let rows = rows_of(&publisher);
    assert_eq!(
        assistant_texts(&rows),
        vec![("msg_a".to_owned(), "Hello wor".to_owned())]
    );
    let row = rows.iter().find(|row| row["id"] == json!("msg_a")).unwrap();
    assert_eq!(row["state"], json!("done"));
    assert_eq!(row["assistantResponseId"], json!("msg_a"));
}

/// 续写只出了半截就失败、用新的 message id 重发：新的那条先放回续写之前的正文再接，不留旧的半截。
#[test]
fn a_retried_continuation_replaces_the_half_written_one() {
    let a = text("msg_a", "Hello wor", StopReason::MaxTokens);
    let m = nudge("msg_m", OUTPUT_LIMIT_RESUME_SOURCE);
    let b2 = text("msg_b2", "ld!", StopReason::EndTurn);
    let (mut projection, mut publisher) = live();
    apply(
        &mut publisher,
        &mut projection,
        vec![
            started(),
            done(user()),
            done(a.clone()),
            done(m.clone()),
            delta("msg_b1", BlockDelta::Text("xx".to_owned())),
            delta("msg_b2", BlockDelta::Text("ld".to_owned())),
        ],
    );
    assert_eq!(
        assistant_texts(&rows_of(&publisher)),
        vec![("msg_a".to_owned(), "Hello world".to_owned())]
    );
    apply(
        &mut publisher,
        &mut projection,
        vec![done(b2.clone()), finished(TurnOutcome::Success)],
    );
    assert_eq!(rows_of(&publisher), cold(vec![user(), a, m, b2]));
}

/// 别的 meta（例如没解析出工具调用时的重试提示）不是续写：下一条 assistant 自己成行。
#[test]
fn another_meta_does_not_join() {
    let messages = vec![
        user(),
        text("msg_a", "Hello wor", StopReason::MaxTokens),
        nudge("msg_m", "malformed_tool_use_retry"),
        text("msg_b", "ld!", StopReason::EndTurn),
    ];
    let (mut projection, mut publisher) = live();
    let mut events = vec![started()];
    events.extend(messages.iter().cloned().map(done));
    events.push(finished(TurnOutcome::Success));
    apply(&mut publisher, &mut projection, events);
    assert_eq!(assistant_texts(&rows_of(&publisher)).len(), 2);
    assert_eq!(rows_of(&publisher), cold(messages));
}
