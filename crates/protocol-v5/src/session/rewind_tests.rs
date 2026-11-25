//! 改写历史的命令、结果与预览的换算（protocol-v5.md §53.2–§53.4）。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::env_rewind::RestoreAction;
use rust_cli_kernel_api::ids::{CommandId, MachineName, MessageId, TurnId};
use rust_cli_kernel_api::input_extras::{AttachedFile, InputRecord, Quote, Relay};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::rewind_preview::{
    PreviewFile, PreviewState, RewindPreview, UnreadableCheckpoint,
};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandResult as KernelResult, Delivery, RewindScope, SessionView, TurnOrigin,
};
use rust_cli_kernel_api::session_facts::TitleSource;
use rust_cli_kernel_api::store::{SessionKind, SessionMeta, TurnRecord};
use serde_json::{Value, json};

use super::super::commands::{accept, finish};
use super::super::lane::{LiveOp, Operation, Outcome, Ticket};
use super::super::projection::{Origin, Projection};
use super::super::state::{self, Gates};
use super::*;
use crate::generated::core_v1::{
    CommandEnvelope, CommandResult, CommandTarget, Expect, ExpectAlt1,
};

const V7: &str = "019a5f3c-7a2b-7c3d-8e4f-0123456789ab";
#[cfg(not(windows))]
const ROOT: &str = "/Users/dev/a";
#[cfg(windows)]
const ROOT: &str = r"C:\Users\dev\a";

fn file() -> AttachedFile {
    AttachedFile {
        path: "/Users/dev/a/notes.txt".to_owned(),
        name: "notes.txt".to_owned(),
        media_type: "text/plain".to_owned(),
        size: 3,
    }
}

fn quote() -> Quote {
    Quote {
        text: "quoted".to_owned(),
        path: None,
        sender_name: None,
        sender_id: None,
        message_id: None,
        sent_at_ms: None,
    }
}

fn view(messages: Vec<Message>, turns: Vec<TurnRecord>) -> SessionView {
    let meta = SessionMeta {
        id: rust_cli_kernel_api::ids::SessionId::new("ses_1"),
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

/// 一轮做完的会话：t1 = u1「first」（带一个本机文件、一条引用、插件代发）→ a1「one」。
fn session() -> (Projection, Publisher) {
    let messages = vec![
        Message::User(UserMessage {
            id: MessageId::new("u1"),
            content: vec![ContentBlock::Text {
                text: "first".to_owned(),
            }],
            origin: UserOrigin::Human,
        }),
        Message::Assistant(AssistantMessage {
            id: MessageId::new("a1"),
            content: vec![ContentBlock::Text {
                text: "one".to_owned(),
            }],
            model: "m".to_owned(),
            stop_reason: None,
            usage: None,
        }),
    ];
    let turns = vec![TurnRecord {
        turn: TurnId::new("t1"),
        first: MessageId::new("u1"),
        origin: TurnOrigin::Input,
        started_at: 1,
        command: Some(CommandId::new("c1")),
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    }];
    let view = SessionView {
        inputs: vec![InputRecord {
            files: vec![file()],
            quotes: vec![quote()],
            relay: Some(Relay::Plugin {
                plugin: "p".to_owned(),
                tool_call: "toolu_p".to_owned(),
            }),
            ..InputRecord::new(MessageId::new("u1"))
        }],
        ..view(messages, turns)
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

fn envelope(kind: &str, row: &str, publisher: &Publisher, payload: Value) -> CommandEnvelope {
    CommandEnvelope {
        command_id: V7.to_owned(),
        r#type: kind.to_owned(),
        target: CommandTarget {
            host_id: "local".to_owned(),
            workspace_key: Some(ROOT.to_owned()),
            session_id: Some("ses_1".to_owned()),
        },
        expect: Some(Expect::Alt1(ExpectAlt1 {
            col: ROWS.to_owned(),
            id: row.to_owned(),
            v: publisher.version(ROWS, row).unwrap_or_default(),
        })),
        payload,
    }
}

fn accepted(
    envelope: &CommandEnvelope,
    projection: &Projection,
    publisher: &Publisher,
) -> Operation {
    match accept(envelope, Some(projection), Some(publisher)) {
        Ok(accepted) => accepted.op,
        Err(result) => panic!("{result:?}"),
    }
}

fn refused_code(
    envelope: &CommandEnvelope,
    projection: &Projection,
    publisher: &Publisher,
) -> String {
    match accept(envelope, Some(projection), Some(publisher)) {
        Err(result) => match *result {
            CommandResult::Rejected(rejected) => rejected.code,
            other => panic!("{other:?}"),
        },
        Ok(_) => panic!("accepted"),
    }
}

fn replaced(op: Operation) -> Command {
    match op {
        Operation::Live(LiveOp::Edit { command, .. }) => command,
        Operation::Live(LiveOp::Kernel(command)) => command,
        _ => panic!("not a kernel command"),
    }
}

/// `turn.edit`：目标是用户输入行；附件与引用没给时沿用原输入的，给了 `[]` 清空；`workspaceMode: rewind` 连文件一起；编辑不带来源。
#[test]
fn an_edit_replaces_the_input_and_keeps_its_attachments_unless_told() {
    let (projection, publisher) = session();
    let edit = envelope(
        types::TURN_EDIT,
        "u1",
        &publisher,
        json!({"newText": "again"}),
    );
    let Command::ReplaceInput {
        target,
        text,
        extras,
        restore_code,
        stop_cut_works,
    } = replaced(accepted(&edit, &projection, &publisher))
    else {
        panic!("not a replacement");
    };
    assert_eq!(
        (target.as_str(), text.as_str(), restore_code),
        ("u1", "again", false)
    );
    // 照 v4 先停被截掉的回合里起的后台工作（§53.12）
    assert!(stop_cut_works);
    assert_eq!(extras.files, [file()]);
    assert_eq!(extras.quotes, [quote()]);
    assert_eq!(extras.relay, None);

    let cleared = envelope(
        types::TURN_EDIT,
        "u1",
        &publisher,
        json!({"newText": "again", "attachments": [], "quotes": [], "workspaceMode": "rewind"}),
    );
    let Command::ReplaceInput {
        extras,
        restore_code,
        ..
    } = replaced(accepted(&cleared, &projection, &publisher))
    else {
        panic!("not a replacement");
    };
    assert!(extras.files.is_empty() && extras.quotes.is_empty());
    assert!(restore_code);

    let empty = envelope(
        types::TURN_EDIT,
        "u1",
        &publisher,
        json!({"newText": "  ", "attachments": [], "quotes": []}),
    );
    assert_eq!(
        refused_code(&empty, &projection, &publisher),
        codes::PRECONDITION_INPUT_EMPTY
    );
    let wrong = envelope(types::TURN_EDIT, "a1", &publisher, json!({"newText": "x"}));
    assert_eq!(
        refused_code(&wrong, &projection, &publisher),
        codes::ADMISSION_ROW_KIND_MISMATCH
    );
}

/// `turn.retry` 在回答上：这一回合开头的输入按它的正文与记录（含来源）重跑；`files.rewind` 在回合头上：回退它起改过的文件。
#[test]
fn retry_and_file_rewind_name_the_opening_input_of_the_turn() {
    let (projection, publisher) = session();
    let retry = envelope(types::TURN_RETRY, "a1", &publisher, json!({}));
    let Command::ReplaceInput {
        target,
        text,
        extras,
        restore_code,
        stop_cut_works,
    } = replaced(accepted(&retry, &projection, &publisher))
    else {
        panic!("not a replacement");
    };
    assert_eq!(
        (target.as_str(), text.as_str(), restore_code),
        ("u1", "first", false)
    );
    assert!(stop_cut_works);
    assert_eq!(extras.files, [file()]);
    assert!(extras.relay.is_some());
    let files = envelope(types::FILES_REWIND, "t1", &publisher, json!({}));
    assert_eq!(
        replaced(accepted(&files, &projection, &publisher)),
        Command::Rewind {
            target: MessageId::new("u1"),
            scope: RewindScope::Code,
        }
    );
    for (kind, row) in [(types::TURN_RETRY, "u1"), (types::FILES_REWIND, "a1")] {
        let wrong = envelope(kind, row, &publisher, json!({}));
        assert_eq!(
            refused_code(&wrong, &projection, &publisher),
            codes::ADMISSION_ROW_KIND_MISMATCH
        );
    }
}

fn ticket(kind: &str) -> Ticket {
    Ticket {
        reply: json!(1),
        id: V7.to_owned(),
        kind: kind.to_owned(),
        entity: None,
    }
}

fn result_of(kind: &str, outcome: Outcome) -> Value {
    json!(finish(&ticket(kind), "ses_1", outcome, None, None, "t").unwrap())
}

/// 结果（§53.2）：改写了 → `rewind`；文件恢复不了 → accepted 的 `blocked` 带预览，重发拿到的同样是它；回退文件恢复不了 → `rewindUnavailable`。
#[test]
fn results_are_rewind_blocked_or_rewind_unavailable() {
    let rewound = Ack::Accepted {
        result: KernelResult::Rewound {
            restored: Vec::new(),
            removed: 2,
        },
    };
    let result = result_of(
        types::TURN_EDIT,
        Outcome::Edited {
            ack: rewound,
            preview: None,
        },
    );
    assert_eq!(result["result"], json!({"disposition": "rewind"}));
    let unavailable = || Ack::Rejected {
        refusal: Refusal::Unavailable,
        reason: "Cannot restore code".to_owned(),
    };
    let preview =
        json!({"canApply": false, "safeFiles": [], "unsafeFiles": [], "ignoredFiles": []});
    let blocked = result_of(
        types::TURN_EDIT,
        Outcome::Edited {
            ack: unavailable(),
            preview: Some(preview.clone()),
        },
    );
    assert_eq!(blocked["kind"], "accepted");
    assert_eq!(
        blocked["result"],
        json!({"disposition": "blocked", "reasonCode": codes::PRECONDITION_REWIND_UNAVAILABLE, "preview": preview})
    );
    let again = result_of(
        types::TURN_EDIT,
        Outcome::Edited {
            ack: Ack::Duplicate {
                original: Box::new(unavailable()),
            },
            preview: None,
        },
    );
    assert_eq!(again["kind"], "duplicate");
    assert_eq!(again["original"]["result"]["disposition"], "blocked");
    let files = result_of(types::FILES_REWIND, Outcome::Ack(unavailable()));
    assert_eq!(files["code"], codes::PRECONDITION_REWIND_UNAVAILABLE);
    // 跨进程的重放只有「接受过」：落进对话的就是改写了
    let replayed = result_of(
        types::TURN_EDIT,
        Outcome::Edited {
            ack: Ack::Duplicate {
                original: Box::new(Ack::Accepted {
                    result: KernelResult::Done,
                }),
            },
            preview: None,
        },
    );
    assert_eq!(replayed["original"]["result"]["disposition"], "rewind");
}

/// 预览的换算（§53.3 的表）：安全的、改过的（不存在写 missing）、读不了的、读不出的 checkpoint；远端的路径带机器名；`canApply` 同 Node。
#[test]
fn a_preview_maps_to_the_contract_shape() {
    let entry = |path: &str, machine: MachineName, state: PreviewState| PreviewFile {
        path: path.to_owned(),
        machine,
        action: RestoreAction::Delete,
        writes: 2,
        tools: vec!["Edit".to_owned(), "Write".to_owned()],
        state,
    };
    let ready = RewindPreview {
        files: vec![entry(
            "/w/b.txt",
            MachineName::primary(),
            PreviewState::Ready,
        )],
        unreadable: Vec::new(),
    };
    assert_eq!(
        preview_result(&ready),
        json!({"canApply": true, "ignoredFiles": [], "unsafeFiles": [],
            "safeFiles": [{"action": "delete", "operationCount": 2, "path": "/w/b.txt", "toolNames": ["Edit", "Write"]}]})
    );
    let mixed = RewindPreview {
        files: vec![
            entry(
                "/w/b.txt",
                MachineName::primary(),
                PreviewState::Changed {
                    expected: Some("e".to_owned()),
                    current: None,
                },
            ),
            entry(
                "/w/c.txt",
                MachineName::new("dev-a"),
                PreviewState::Unreadable {
                    message: "not connected".to_owned(),
                },
            ),
        ],
        unreadable: vec![UnreadableCheckpoint {
            checkpoint: "checkpoint_x".to_owned(),
            message: "gone".to_owned(),
        }],
    };
    let mapped = preview_result(&mixed);
    assert_eq!(mapped["canApply"], false);
    let unsafe_files = mapped["unsafeFiles"].as_array().unwrap();
    let summary: Vec<(&str, &str)> = unsafe_files
        .iter()
        .map(|item| {
            (
                item["path"].as_str().unwrap(),
                item["reason"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("/w/b.txt", "external_modified"),
            ("checkpoint:checkpoint_x", "checkpoint_unreadable"),
            ("dev-a:/w/c.txt", "file_read_failed"),
        ]
    );
    assert_eq!(unsafe_files[0]["currentHash"], "missing");
    assert_eq!(unsafe_files[0]["expectedHash"], "e");
}

/// 门（§53.4）：编辑与重试在空闲和回合在跑时都放开（内核抢占），有压缩时 `compacting`；回退文件只在空闲时；草稿都不行。
#[test]
fn the_rewrite_gates_follow_the_kernel() {
    let turn = TurnId::new("t1");
    let gates = |running: Option<&TurnId>, compaction: bool, draft: bool| {
        state::admission(&Gates {
            running,
            held: false,
            frozen: false,
            deleted: false,
            draft,
            compaction,
            follow_up: Delivery::Queue,
            side_chat: false,
        })
    };
    let idle = gates(None, false, false);
    for gate in ["edit", "retry", "rewindFiles"] {
        assert_eq!(idle[gate], json!({"allow": true}), "{gate}");
    }
    let running = gates(Some(&turn), false, false);
    assert_eq!(running["edit"], json!({"allow": true}));
    assert_eq!(running["retry"], json!({"allow": true}));
    assert_eq!(
        running["rewindFiles"],
        json!({"allow": false, "reason": codes::ADMISSION_BUSY})
    );
    let compacting = gates(Some(&turn), true, false);
    assert_eq!(
        compacting["edit"],
        json!({"allow": false, "reason": codes::ADMISSION_COMPACTING})
    );
    let draft = gates(None, false, true);
    assert_eq!(
        draft["edit"],
        json!({"allow": false, "reason": codes::ADMISSION_DRAFT})
    );
    assert_eq!(
        draft["rewindFiles"],
        json!({"allow": false, "reason": codes::ADMISSION_DRAFT})
    );
}
