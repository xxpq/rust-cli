//! `row.reportHighspeed`（protocol-v5.md §46.2）：行在、带高速卡 → 内核的 `PatchNote`；行不在 → notFound；不带 → notHighspeed；不比 `v`。
//! 内核核对出没有注记（投影落后于内核）同样回 notHighspeed。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::{MessageId, SessionId, TraceId};
use rust_cli_kernel_api::input_extras::InputRecord;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{Ack, SessionView};
use rust_cli_kernel_api::session_facts::TitleSource;
use rust_cli_kernel_api::store::{SessionKind, SessionMeta};
use serde_json::{Value, json};

use super::*;
use crate::generated::core_v1::{CommandEnvelope, CommandTarget, ExpectAlt1};
use crate::session::projection::Origin;
use crate::session::rows::ROWS;

const V7: &str = "019a5f3c-7a2b-7c3d-8e4f-0123456789ab";
#[cfg(not(windows))]
const ROOT: &str = "/Users/dev/a";
#[cfg(windows)]
const ROOT: &str = r"C:\Users\dev\a";

fn metrics() -> Value {
    json!({"regularTps": 40, "outputTokens": 120, "durationMs": 3_000, "highspeedTps": 160, "savedDurationMs": 900})
}

fn report(row: &str, v: u64, metrics: Value) -> CommandEnvelope {
    CommandEnvelope {
        command_id: V7.to_owned(),
        r#type: types::ROW_REPORT_HIGHSPEED.to_owned(),
        target: CommandTarget {
            host_id: "local".to_owned(),
            workspace_key: Some(ROOT.to_owned()),
            session_id: Some("ses_1".to_owned()),
        },
        expect: Some(Expect::Alt1(ExpectAlt1 {
            col: ROWS.to_owned(),
            id: row.to_owned(),
            v,
        })),
        payload: json!({ "metrics": metrics }),
    }
}

fn user(id: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

/// 两条输入：`msg_card` 带高速卡（注记），`msg_plain` 不带。
fn session() -> (Projection, crate::publisher::Publisher) {
    let meta = SessionMeta {
        id: SessionId::new("ses_1"),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: rust_cli_kernel_api::env::WorkspaceRef {
            environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
            root: rust_cli_kernel_api::env::EnvPath::new(ROOT),
        },
        title: String::new(),
        trace: TraceId::new("ses_1"),
        created_at_ms: 1,
    };
    let view = SessionView {
        inputs: vec![InputRecord {
            note: Some(json!({"schemaVersion": 1, "cardId": "hsc_1"})),
            ..InputRecord::from_entry("msg_card", &json!({})).unwrap()
        }],
        ..SessionView::new(
            meta,
            vec![user("msg_card"), user("msg_plain")],
            rust_cli_kernel_api::testing::selection(),
            PermissionMode::Default,
        )
    };
    let origin = Origin {
        live: true,
        draft: false,
        deleted: false,
        title_source: TitleSource::Default,
        mcp: Value::Null,
        subagents: Vec::new(),
        stamp: None,
        ..Default::default()
    };
    let (projection, content) = Projection::build(&view, origin);
    let mut publisher = crate::publisher::Publisher::new("ws/a/session/ses_1", "e");
    publisher.adopt(content);
    (projection, publisher)
}

fn rejected_code(result: Result<Accepted, Refused>) -> String {
    match result {
        Err(result) => match *result {
            CommandResult::Rejected(rejected) => rejected.code,
            other => panic!("not rejected: {other:?}"),
        },
        Ok(_) => panic!("accepted"),
    }
}

#[test]
fn a_report_on_a_card_row_patches_its_note_without_comparing_v() {
    let (projection, publisher) = session();
    let v = publisher.version(ROWS, "msg_card").unwrap();
    // 不比 v（entityNoVersion，后写胜）：旧的 v 照样交给内核
    let Ok(Accepted {
        op: Operation::Live(LiveOp::Kernel(command)),
        ..
    }) = accept(
        &report("msg_card", v + 7, metrics()),
        Some(&projection),
        Some(&publisher),
    )
    else {
        panic!("not a kernel command");
    };
    assert_eq!(
        command,
        Command::PatchNote {
            message: MessageId::new("msg_card"),
            patch: metrics().as_object().unwrap().clone(),
        }
    );
}

#[test]
fn a_report_needs_a_card_row_and_typed_metrics() {
    let (projection, publisher) = session();
    let run = |envelope: &CommandEnvelope| accept(envelope, Some(&projection), Some(&publisher));
    assert_eq!(
        rejected_code(run(&report("msg_gone", 1, metrics()))),
        codes::PRECONDITION_NOT_FOUND
    );
    let v = publisher.version(ROWS, "msg_plain").unwrap();
    assert_eq!(
        rejected_code(run(&report("msg_plain", v, metrics()))),
        codes::PRECONDITION_NOT_HIGHSPEED
    );
    let mut partial = metrics();
    partial.as_object_mut().unwrap().remove("highspeedTps");
    let v = publisher.version(ROWS, "msg_card").unwrap();
    assert_eq!(
        rejected_code(run(&report("msg_card", v, partial))),
        codes::PRECONDITION_INVALID_PAYLOAD
    );
}

/// 投影落后于内核（行刚被改写、注记不在了）：内核的 `Unavailable` 回 notHighspeed。
#[test]
fn the_kernel_saying_no_note_is_not_highspeed() {
    let ticket = Ticket {
        reply: json!(1),
        id: V7.to_owned(),
        kind: types::ROW_REPORT_HIGHSPEED.to_owned(),
        entity: None,
    };
    let result = finish(
        &ticket,
        "ses_1",
        Outcome::Ack(Ack::Rejected {
            refusal: Refusal::Unavailable,
            reason: "the input has no note".to_owned(),
        }),
        None,
        None,
        "t",
    )
    .unwrap();
    let CommandResult::Rejected(rejected) = result else {
        panic!("not rejected");
    };
    assert_eq!(rejected.code, codes::PRECONDITION_NOT_HIGHSPEED);
}
