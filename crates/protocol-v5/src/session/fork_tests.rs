//! fork 的命令、结果与门的换算（protocol-v5.md §55.2、§54.5）。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::{CommandId, MessageId, SessionId, TurnId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandResult as KernelResult, Delivery, Forked, SessionView, TurnOrigin,
};
use rust_cli_kernel_api::session_facts::TitleSource;
use rust_cli_kernel_api::store::{SessionKind, SessionMeta, TurnRecord};
use serde_json::{Value, json};

use super::super::commands::{accept, finish, types};
use super::super::lane::{LiveOp, Operation, Outcome, Ticket};
use super::super::projection::{Origin, Projection};
use super::super::rows::ROWS;
use super::super::state::{self, Gates};
use crate::generated::contract::codes;
use crate::generated::core_v1::{
    CommandEnvelope, CommandResult, CommandTarget, Expect, ExpectAlt1,
};
use crate::publisher::Publisher;

const V7: &str = "019a5f3c-7a2b-7c3d-8e4f-0123456789ab";
#[cfg(not(windows))]
const ROOT: &str = "/Users/dev/a";
#[cfg(windows)]
const ROOT: &str = r"C:\Users\dev\a";

fn text(id: &str, origin: UserOrigin, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin,
    })
}

fn answer(id: &str, text: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn turn(id: &str, first: &str, origin: TurnOrigin) -> TurnRecord {
    TurnRecord {
        turn: TurnId::new(id),
        first: MessageId::new(first),
        origin,
        started_at: 1,
        command: Some(CommandId::new("c1")),
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    }
}

/// 两轮做完的会话：t1 = u1「first」→ a1「one」；t2 是后台通知开的（n1 是给模型的通知，不出行）→ a2「noted」。
fn session() -> (Projection, Publisher) {
    let messages = vec![
        text("u1", UserOrigin::Human, "first"),
        answer("a1", "one"),
        text(
            "n1",
            UserOrigin::Meta {
                source: "task-notification".to_owned(),
            },
            "<task-notification>done</task-notification>",
        ),
        answer("a2", "noted"),
    ];
    let meta = SessionMeta {
        id: SessionId::new("ses_1"),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: rust_cli_kernel_api::env::WorkspaceRef {
            environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
            root: rust_cli_kernel_api::env::EnvPath::new(ROOT),
        },
        title: String::new(),
        trace: rust_cli_kernel_api::ids::TraceId::new("ses_1"),
        created_at_ms: 1,
    };
    let view = SessionView {
        turns: vec![
            turn("t1", "u1", TurnOrigin::Input),
            turn("t2", "n1", TurnOrigin::Notice),
        ],
        ..SessionView::new(
            meta,
            messages,
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
    let mut publisher = Publisher::new("ws/a/session/ses_1", "e");
    publisher.adopt(content);
    (projection, publisher)
}

fn envelope(row: Option<&str>, publisher: &Publisher, payload: Value) -> CommandEnvelope {
    CommandEnvelope {
        command_id: V7.to_owned(),
        r#type: types::TURN_FORK.to_owned(),
        target: CommandTarget {
            host_id: "local".to_owned(),
            workspace_key: Some(ROOT.to_owned()),
            session_id: Some("ses_1".to_owned()),
        },
        expect: row.map(|row| {
            Expect::Alt1(ExpectAlt1 {
                col: ROWS.to_owned(),
                id: row.to_owned(),
                v: publisher.version(ROWS, row).unwrap_or_default(),
            })
        }),
        payload,
    }
}

fn refused(envelope: &CommandEnvelope, projection: &Projection, publisher: &Publisher) -> String {
    match accept(envelope, Some(projection), Some(publisher)) {
        Err(result) => match *result {
            CommandResult::Rejected(rejected) => rejected.code,
            other => panic!("{other:?}"),
        },
        Ok(_) => panic!("accepted"),
    }
}

/// `turn.fork` 在回答上：fork 到这一回合为止，内核的命令指名这一回合开头的输入与调用方给的新会话 id（§54.2）。
#[test]
fn a_fork_names_the_opening_input_of_the_answers_turn() {
    let (projection, publisher) = session();
    let fork = envelope(Some("a1"), &publisher, json!({"sessionId": "ses_child"}));
    let op = match accept(&fork, Some(&projection), Some(&publisher)) {
        Ok(accepted) => accepted.op,
        Err(result) => panic!("{result:?}"),
    };
    let Operation::Live(LiveOp::Kernel(command)) = op else {
        panic!("not a kernel command");
    };
    assert_eq!(
        command,
        Command::ForkFromInputAs {
            input: MessageId::new("u1"),
            session: SessionId::new("ses_child"),
        }
    );
}

/// 目标不是回答 → `rowKindMismatch`；回合没有开头的输入（通知开的）→ `notFound`；没有 expect、参数不对 → `invalidPayload`。
#[test]
fn a_fork_needs_an_answer_with_an_opening_input() {
    let (projection, publisher) = session();
    let payload = json!({"sessionId": "ses_child"});
    let input = envelope(Some("u1"), &publisher, payload.clone());
    assert_eq!(
        refused(&input, &projection, &publisher),
        codes::ADMISSION_ROW_KIND_MISMATCH
    );
    let noted = envelope(Some("a2"), &publisher, payload.clone());
    assert_eq!(
        refused(&noted, &projection, &publisher),
        codes::PRECONDITION_NOT_FOUND
    );
    let bare = envelope(None, &publisher, payload);
    assert_eq!(
        refused(&bare, &projection, &publisher),
        codes::PRECONDITION_INVALID_PAYLOAD
    );
    for bad in [json!({}), json!({"sessionId": "s", "extra": 1})] {
        let wrong = envelope(Some("a1"), &publisher, bad);
        assert_eq!(
            refused(&wrong, &projection, &publisher),
            codes::PRECONDITION_INVALID_PAYLOAD
        );
    }
}

fn result_of(outcome: Outcome) -> Value {
    let ticket = Ticket {
        reply: json!(1),
        id: V7.to_owned(),
        kind: types::TURN_FORK.to_owned(),
        entity: None,
    };
    json!(finish(&ticket, "ses_1", outcome, None, None, "t").unwrap())
}

/// 结果（§54.2）：分出了（第一次与已有的同一个结果）→ `{sessionId}`；重放按原结果；id 是别的会话 → `sessionExists`；忙 → `busy`。
#[test]
fn results_carry_the_forked_session() {
    let forked = |through: Option<&str>, copied| Ack::Accepted {
        result: KernelResult::Forked(Forked {
            session: SessionId::new("ses_child"),
            through: through.map(MessageId::new),
            copied,
        }),
    };
    for ack in [forked(Some("a1"), 2), forked(None, 0)] {
        let result = result_of(Outcome::Ack(ack));
        assert_eq!(result["kind"], "accepted");
        assert_eq!(result["result"], json!({"sessionId": "ses_child"}));
    }
    let again = result_of(Outcome::Ack(Ack::Duplicate {
        original: Box::new(forked(Some("a1"), 2)),
    }));
    assert_eq!(again["kind"], "duplicate");
    assert_eq!(
        again["original"]["result"],
        json!({"sessionId": "ses_child"})
    );
    let rejected = |refusal| {
        result_of(Outcome::Ack(Ack::Rejected {
            refusal,
            reason: "r".to_owned(),
        }))["code"]
            .clone()
    };
    assert_eq!(
        rejected(Refusal::Unavailable),
        codes::PRECONDITION_SESSION_EXISTS
    );
    assert_eq!(rejected(Refusal::Busy), codes::ADMISSION_BUSY);
    assert_eq!(rejected(Refusal::NotFound), codes::PRECONDITION_NOT_FOUND);
}

/// 门（§54.5）：空闲与回合在跑时都放开（fork 不打断在跑的回合），有压缩时 `compacting`，草稿 `draft`，删除了 `sessionDeleted`。
#[test]
fn the_fork_gate_follows_the_kernel() {
    let turn = TurnId::new("t1");
    let gate = |running: Option<&TurnId>, compaction: bool, draft: bool, deleted: bool| {
        state::admission(&Gates {
            running,
            held: false,
            frozen: false,
            deleted,
            draft,
            compaction,
            follow_up: Delivery::Queue,
            side_chat: false,
        })["fork"]
            .clone()
    };
    assert_eq!(gate(None, false, false, false), json!({"allow": true}));
    assert_eq!(
        gate(Some(&turn), false, false, false),
        json!({"allow": true})
    );
    assert_eq!(
        gate(Some(&turn), true, false, false),
        json!({"allow": false, "reason": codes::ADMISSION_COMPACTING})
    );
    assert_eq!(
        gate(None, false, true, false),
        json!({"allow": false, "reason": codes::ADMISSION_DRAFT})
    );
    assert_eq!(
        gate(None, false, false, true),
        json!({"allow": false, "reason": codes::ADMISSION_SESSION_DELETED})
    );
}
