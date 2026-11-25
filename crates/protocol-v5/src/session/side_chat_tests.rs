//! 侧聊的命令、结果、门与投影（protocol-v5.md §57.2、§57.4、§57.5）。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandResult as KernelResult, Delivery, Forked, SessionView,
};
use rust_cli_kernel_api::session_facts::TitleSource;
use rust_cli_kernel_api::store::{SessionKind, SessionMeta};
use serde_json::{Value, json};

use super::super::commands::{accept, finish, types};
use super::super::lane::{Operation, Outcome, Ticket};
use super::super::projection::{Origin, Projection};
use super::super::rows::ROWS;
use super::super::state::{self, Gates};
use super::{forbidden, shown};
use crate::generated::contract::codes;
use crate::generated::core_v1::{CommandEnvelope, CommandResult, CommandTarget};
use crate::publisher::Publisher;

const V7: &str = "019a5f3c-7a2b-7c3d-8e4f-0123456789ab";
#[cfg(not(windows))]
const ROOT: &str = "/Users/dev/a";
#[cfg(windows)]
const ROOT: &str = r"C:\Users\dev\a";

fn create(payload: Value) -> CommandEnvelope {
    CommandEnvelope {
        command_id: V7.to_owned(),
        r#type: types::SESSION_CREATE.to_owned(),
        target: CommandTarget {
            host_id: "local".to_owned(),
            workspace_key: Some(ROOT.to_owned()),
            session_id: Some("ses_parent".to_owned()),
        },
        expect: None,
        payload,
    }
}

fn refused(envelope: &CommandEnvelope) -> (String, Option<String>) {
    match accept(envelope, None, None) {
        Err(result) => match *result {
            CommandResult::Rejected(rejected) => (rejected.code, rejected.field),
            other => panic!("{other:?}"),
        },
        Ok(_) => panic!("accepted"),
    }
}

/// 侧聊落在父会话的通道里（`parent`），内核的命令带调用方给的 id；首条输入按 `input.send` 换成同一个内核命令。
#[test]
fn a_side_chat_runs_in_the_parents_lane() {
    let accepted = accept(
        &create(json!({"sessionId": "ses_side", "kind": "sideChat", "parent": "ses_parent"})),
        None,
        None,
    )
    .unwrap_or_else(|result| panic!("{result:?}"));
    assert_eq!(accepted.sid, "ses_parent");
    let Operation::SideChat { command, first } = accepted.op else {
        panic!("not a side chat");
    };
    assert_eq!(
        command,
        Command::SideChatAs {
            session: SessionId::new("ses_side"),
        }
    );
    assert!(first.is_none());
    let with_input = accept(
        &create(json!({"sessionId": "ses_side", "kind": "sideChat", "parent": "ses_parent",
            "firstInput": {"text": "hello", "modelSelection": {"providerId": "p", "modelId": "m"}}})),
        None,
        None,
    )
    .unwrap_or_else(|result| panic!("{result:?}"));
    let Operation::SideChat {
        first: Some(Command::SubmitFollowUp { text, extras }),
        ..
    } = with_input.op
    else {
        panic!("the first input is not a submit");
    };
    assert_eq!(text, "hello");
    assert_eq!(
        extras.config.selection.map(|selection| selection.model),
        Some("m".to_owned())
    );
}

/// 没有 `parent` → `invalidPayload{parent}`；带 `importedHistory` → `capabilityUnsupported`；首条输入读不通 → 字段前加 `firstInput`；
/// 空的首条输入 → `inputEmpty`。
#[test]
fn a_side_chat_needs_a_parent_and_a_valid_first_input() {
    assert_eq!(
        refused(&create(json!({"sessionId": "s", "kind": "sideChat"}))),
        (
            codes::PRECONDITION_INVALID_PAYLOAD.to_owned(),
            Some("parent".to_owned())
        )
    );
    assert_eq!(
        refused(&create(
            json!({"sessionId": "s", "kind": "sideChat", "parent": "p",
            "importedHistory": []})
        ))
        .0,
        codes::PRECONDITION_CAPABILITY_UNSUPPORTED
    );
    let (code, field) = refused(&create(
        json!({"sessionId": "s", "kind": "sideChat", "parent": "p", "firstInput": {"text": 1}}),
    ));
    assert_eq!(code, codes::PRECONDITION_INVALID_PAYLOAD);
    assert!(field.is_some_and(|field| field.starts_with("firstInput")));
    assert_eq!(
        refused(&create(
            json!({"sessionId": "s", "kind": "sideChat", "parent": "p",
            "firstInput": {"text": "  "}})
        ))
        .0,
        codes::PRECONDITION_INPUT_EMPTY
    );
    // 主会话的首条输入照旧不接受
    assert_eq!(
        refused(&create(
            json!({"sessionId": "s", "firstInput": {"text": "x"}})
        ))
        .0,
        codes::PRECONDITION_CAPABILITY_UNSUPPORTED
    );
}

fn result_of(sid: &str, ack: Ack) -> Value {
    let ticket = Ticket {
        reply: json!(1),
        id: V7.to_owned(),
        kind: types::SESSION_CREATE.to_owned(),
        entity: None,
    };
    json!(finish(&ticket, sid, Outcome::Ack(ack), None, None, "t").unwrap())
}

/// 结果（§57.2）：父会话分出的会话（第一次与已有同一个结果）、侧聊里首条输入的回执都是 `{sessionId: 侧聊}`；重放按原结果；id 是别的会话 → `sessionExists`。
#[test]
fn results_carry_the_side_chat() {
    let forked = |copied| Ack::Accepted {
        result: KernelResult::Forked(Forked {
            session: SessionId::new("ses_side"),
            through: None,
            copied,
        }),
    };
    for ack in [forked(3), forked(0)] {
        let result = result_of("ses_parent", ack);
        assert_eq!(result["kind"], "accepted");
        assert_eq!(result["result"], json!({"sessionId": "ses_side"}));
    }
    let first_input = result_of(
        "ses_side",
        Ack::Accepted {
            result: KernelResult::Done,
        },
    );
    assert_eq!(first_input["result"], json!({"sessionId": "ses_side"}));
    let again = result_of(
        "ses_parent",
        Ack::Duplicate {
            original: Box::new(forked(3)),
        },
    );
    assert_eq!(again["kind"], "duplicate");
    assert_eq!(
        again["original"]["result"],
        json!({"sessionId": "ses_side"})
    );
    let taken = result_of(
        "ses_parent",
        Ack::Rejected {
            refusal: Refusal::Unavailable,
            reason: "r".to_owned(),
        },
    );
    assert_eq!(taken["code"], codes::PRECONDITION_SESSION_EXISTS);
}

/// 侧聊会话不接受的一步（§57.4）：禁止表里的命令，与以它为父会话再开侧聊；首条输入（同一个 `session.create`，但只是提交）照常。
#[test]
fn the_forbidden_steps_on_a_side_chat() {
    let side_chat = Operation::SideChat {
        command: Command::SideChatAs {
            session: SessionId::new("s"),
        },
        first: None,
    };
    let submit = || {
        Operation::Live(super::super::lane::LiveOp::Kernel(
            Command::SubmitFollowUp {
                text: "x".to_owned(),
                extras: Default::default(),
            },
        ))
    };
    assert!(forbidden(types::SESSION_CREATE, &side_chat));
    assert!(!forbidden(types::SESSION_CREATE, &submit()));
    for kind in [
        types::TURN_FORK,
        types::TURN_EDIT,
        types::TURN_RETRY,
        types::FILES_REWIND,
        types::SESSION_RENAME,
        types::SESSION_DELETE,
    ] {
        assert!(forbidden(kind, &submit()), "{kind}");
    }
    // 压缩照 v4 允许（§57.11 审定第 1 项）
    for kind in [
        types::SESSION_COMPACT,
        types::INPUT_SEND,
        types::TURN_STOP,
        types::INTERACTION_RESOLVE,
    ] {
        assert!(!forbidden(kind, &submit()), "{kind}");
    }
}

/// 门（§57.4）：侧聊会话的 fork、edit、retry、rewindFiles 写 `admission.sideChat`；压缩（照 v4，§57.11）、输入与停止照常。
#[test]
fn the_gates_of_a_side_chat() {
    let gates = state::admission(&Gates {
        running: None,
        held: false,
        frozen: false,
        deleted: false,
        draft: false,
        compaction: false,
        follow_up: Delivery::Queue,
        side_chat: true,
    });
    for gate in ["fork", "edit", "retry", "rewindFiles"] {
        assert_eq!(
            gates[gate],
            json!({"allow": false, "reason": codes::ADMISSION_SIDE_CHAT}),
            "{gate}"
        );
    }
    assert_eq!(gates["compact"], json!({"allow": true}));
    assert_eq!(gates["input"]["mode"], "startNow");
    assert_eq!(gates["queueSendNow"], json!({"allow": true}));
}

fn user(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Human,
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

fn view(kind: SessionKind) -> SessionView {
    let meta = SessionMeta {
        id: SessionId::new("ses_side"),
        kind,
        parent: Some(SessionId::new("ses_parent")),
        workspace: rust_cli_kernel_api::env::WorkspaceRef {
            environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
            root: rust_cli_kernel_api::env::EnvPath::new(ROOT),
        },
        title: "Selection side chat".to_owned(),
        trace: rust_cli_kernel_api::ids::TraceId::new("ses_parent"),
        created_at_ms: 1,
    };
    let messages = vec![
        user("u1", "inherited question"),
        answer("a1", "inherited answer"),
        Message::Attachment(AttachmentMessage {
            id: MessageId::new("b1"),
            source: rust_cli_kernel_api::store::side_chat::SOURCE.to_owned(),
            text: "<system-reminder>\nboundary\n</system-reminder>".to_owned(),
            data: Value::Null,
        }),
        user("u2", "side question"),
        answer("a2", "side answer"),
    ];
    SessionView::new(
        meta,
        messages,
        rust_cli_kernel_api::testing::selection(),
        PermissionMode::Default,
    )
}

/// 投影（§57.5）：侧聊从边界之后成行，`meta.kind = sideChat`、`rowsWindow` 不数继承的历史；别的会话照常全部成行。
#[test]
fn a_side_chat_shows_only_what_follows_the_boundary() {
    let side = view(SessionKind::SideChat);
    assert_eq!(
        shown(&side)
            .iter()
            .map(|message| message.id().as_str())
            .collect::<Vec<_>>(),
        ["u2", "a2"]
    );
    assert_eq!(shown(&view(SessionKind::Fork)).len(), 5);
    let origin = Origin {
        live: false,
        draft: false,
        deleted: false,
        title_source: TitleSource::Generated,
        mcp: Value::Null,
        subagents: Vec::new(),
        stamp: None,
        ..Default::default()
    };
    let (_, content) = Projection::build(&side, origin);
    let mut publisher = Publisher::new("ws/a/session/ses_side", "e");
    publisher.adopt(content);
    let mut texts: Vec<&str> = publisher
        .entities(ROWS)
        .filter_map(|row| row.get("text").and_then(Value::as_str))
        .collect();
    texts.sort_unstable();
    assert_eq!(texts, ["side answer", "side question"]);
    let state = publisher.state();
    assert_eq!(state["meta"]["kind"], "sideChat");
    // 侧聊里没有回合记录的那条输入另开一个回合（§75.3）：回合头、输入、回答
    assert_eq!(state["rowsWindow"]["totalCount"], 3);
    assert_eq!(
        state["admission"]["fork"]["reason"],
        codes::ADMISSION_SIDE_CHAT
    );
}
