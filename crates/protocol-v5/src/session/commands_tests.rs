//! 会话命令的校验与回执的换算（protocol-v5.md §15.4）。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::{CommandId, TurnId};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::Ack;
use serde_json::{Value, json};

use super::*;
use crate::generated::core_v1::{CommandEnvelope, CommandTarget};

const V7: &str = "019a5f3c-7a2b-7c3d-8e4f-0123456789ab";
#[cfg(not(windows))]
const ROOT: &str = "/Users/dev/a";
#[cfg(windows)]
const ROOT: &str = r"C:\Users\dev\a";

fn envelope(kind: &str, payload: Value) -> CommandEnvelope {
    CommandEnvelope {
        command_id: V7.to_owned(),
        r#type: kind.to_owned(),
        target: CommandTarget {
            host_id: "local".to_owned(),
            workspace_key: Some(ROOT.to_owned()),
            session_id: Some("ses_1".to_owned()),
        },
        expect: None,
        payload,
    }
}

fn code(result: &CommandResult) -> (&str, Option<&str>) {
    match result {
        CommandResult::Rejected(rejected) => (rejected.code.as_str(), rejected.field.as_deref()),
        other => panic!("not rejected: {other:?}"),
    }
}

fn refused(envelope: &CommandEnvelope) -> Box<CommandResult> {
    match accept(envelope, None, None) {
        Err(result) => result,
        Ok(_) => panic!("accepted"),
    }
}

#[test]
fn a_session_command_id_must_be_a_uuid_v7() {
    let mut send = envelope(types::INPUT_SEND, json!({"text": "hi"}));
    send.command_id = "window-1-1".to_owned();
    assert_eq!(
        code(&refused(&send)),
        (codes::PRECONDITION_INVALID_COMMAND_ID, None)
    );
}

#[test]
fn inputs_are_checked_before_they_reach_the_kernel() {
    // 丢掉不支持的字段之后没有内容（C1）
    let empty = envelope(
        types::INPUT_SEND,
        json!({"text": "  ", "attachments": [{"ref": "x"}]}),
    );
    assert_eq!(
        code(&refused(&empty)),
        (codes::PRECONDITION_INPUT_EMPTY, None)
    );
    // 插话要指名回合（02 §4.2）
    let guide = envelope(
        types::INPUT_SEND,
        json!({"text": "hi", "disposition": "guide"}),
    );
    assert_eq!(
        code(&refused(&guide)),
        (codes::PRECONDITION_INVALID_PAYLOAD, Some("turnId"))
    );
    // 每轮的工具禁用名单（K-C2）：接受并交给内核（以前内核没做时拒绝，02 §12.9 审定做完后放开）
    let limited = envelope(
        types::INPUT_SEND,
        json!({"text": "hi", "toolDisallowlist": ["Bash"]}),
    );
    let Ok(Accepted {
        op: Operation::Live(LiveOp::Kernel(Command::SubmitFollowUp { extras, .. })),
        ..
    }) = accept(&limited, None, None)
    else {
        panic!("not a submit");
    };
    assert_eq!(extras.disallowed_tools, ["Bash"]);
    // 参数严格：未知字段带路径
    let unknown = envelope(types::INPUT_SEND, json!({"text": "hi", "colour": "red"}));
    assert_eq!(
        code(&refused(&unknown)),
        (codes::PRECONDITION_INVALID_PAYLOAD, Some("colour"))
    );
    // 能力里没做的命令（`config.setFollowupMode` FIX-SEND 做了，§21）
    let discard = envelope("context.discardShared", json!({"contextId": "ctx_1"}));
    assert_eq!(
        code(&refused(&discard)).0,
        codes::PRECONDITION_CAPABILITY_UNSUPPORTED
    );
}

/// 只发附件不是空输入（02 §11.2 C1 的 bug）：附件进内核的 `Submit`（§15.4.1）。
#[test]
fn an_input_of_only_a_local_file_reaches_the_kernel() {
    let file = std::path::Path::new(ROOT).join("notes.md");
    let send = envelope(
        types::INPUT_SEND,
        json!({"text": "", "attachments": [{"ref": file, "fileName": "notes.md",
            "mime": "text/markdown", "bytes": 12}]}),
    );
    let Ok(accepted) = accept(&send, None, None) else {
        panic!("rejected");
    };
    let Operation::Live(LiveOp::Kernel(Command::SubmitFollowUp { text, extras })) = accepted.op
    else {
        panic!("not a submit");
    };
    assert!(text.is_empty());
    assert_eq!(extras.files.len(), 1);
    assert_eq!(std::path::Path::new(&extras.files[0].path), file);
}

/// 只带引用的输入同样有内容（K-P1）；只带来源的没有（来源只供显示）。
#[test]
fn a_quote_is_content_and_a_source_alone_is_not() {
    let quoted = envelope(
        types::INPUT_SEND,
        json!({"text": "", "quotes": [{"text": "q"}]}),
    );
    assert!(accept(&quoted, None, None).is_ok());
    let relayed = envelope(
        types::INPUT_SEND,
        json!({"text": " ", "source": {"kind": "genUi", "path": "/Users/dev/a.html"}}),
    );
    assert_eq!(
        code(&refused(&relayed)),
        (codes::PRECONDITION_INPUT_EMPTY, None)
    );
}

/// `input.send` 的输入框的配置进内核的 `extras.config`（K-C1-INTENT，protocol-v5.md §18.1）：模式名照 v4 的 `resolveExecutionState`
/// 换（给了模式、没给 plan 时 plan 关；旧写法 `plan` 是 plan 开）。
#[test]
fn the_composer_config_reaches_the_kernel() {
    let config = |payload: Value| {
        let Ok(Accepted {
            op: Operation::Live(LiveOp::Kernel(Command::SubmitFollowUp { extras, .. })),
            ..
        }) = accept(&envelope(types::INPUT_SEND, payload), None, None)
        else {
            panic!("not a submit");
        };
        extras.config
    };
    let given = config(json!({"text": "hi", "mode": "guarded", "planEnabled": true,
        "modelSelection": {"providerId": "p", "modelId": "m", "options": {"reasoningLevel": "high"}}}));
    assert_eq!(given.mode, Some(PermissionMode::Guarded));
    assert_eq!(given.plan, Some(true));
    let chosen = given.selection.unwrap();
    assert_eq!(
        (
            chosen.provider.as_str(),
            chosen.model.as_str(),
            chosen.reasoning.as_deref()
        ),
        ("p", "m", Some("high"))
    );
    let mode_only = config(json!({"text": "hi", "mode": "yolo"}));
    assert_eq!(
        (mode_only.mode, mode_only.plan),
        (Some(PermissionMode::BypassPermissions), Some(false))
    );
    let legacy = config(json!({"text": "hi", "mode": "plan"}));
    assert_eq!((legacy.mode, legacy.plan), (None, Some(true)));
    let plan_only = config(json!({"text": "hi", "planEnabled": false}));
    assert_eq!((plan_only.mode, plan_only.plan), (None, Some(false)));
    assert!(config(json!({"text": "hi"})).is_unset());
    let unknown = envelope(types::INPUT_SEND, json!({"text": "hi", "mode": "sideways"}));
    assert_eq!(
        code(&refused(&unknown)),
        (codes::PRECONDITION_INVALID_MODE, Some("mode"))
    );
}

/// 对扣住的队列的处置是一条内核命令，带上确认框看到的队列（§15.5，内核与出队同一步核对）。
#[test]
fn a_held_queue_disposition_carries_the_queue_it_was_shown() {
    for (disposition, keep) in [("keepQueue", true), ("discardQueue", false)] {
        let send = envelope(
            types::INPUT_SEND,
            json!({"text": "now", "disposition": disposition, "expectedHeldQueueItemIds": ["c2"]}),
        );
        let Ok(Accepted {
            op: Operation::Live(LiveOp::Kernel(command)),
            ..
        }) = accept(&send, None, None)
        else {
            panic!("not a kernel command");
        };
        let expected = Some(vec![rust_cli_kernel_api::ids::CommandId::new("c2")]);
        match command {
            Command::SubmitKeepingQueue { expected_held, .. } if keep => {
                assert_eq!(expected_held, expected);
            }
            Command::SubmitDiscardingQueue { expected_held, .. } if !keep => {
                assert_eq!(expected_held, expected);
            }
            other => panic!("{disposition}: {other:?}"),
        }
    }
}

#[test]
fn a_queue_or_interaction_command_needs_its_entity() {
    let mut delete = envelope(types::QUEUE_DELETE, json!({}));
    assert_eq!(
        code(&refused(&delete)),
        (codes::PRECONDITION_INVALID_PAYLOAD, Some("expect"))
    );
    delete.expect = Some(Expect::Alt1(crate::generated::core_v1::ExpectAlt1 {
        col: "queue".to_owned(),
        id: "c2".to_owned(),
        v: 1,
    }));
    assert_eq!(code(&refused(&delete)).0, codes::PRECONDITION_NOT_FOUND);
    let mut resolve = envelope(types::INTERACTION_RESOLVE, json!({"answer": {}}));
    resolve.expect = Some(Expect::Alt1(crate::generated::core_v1::ExpectAlt1 {
        col: "interactions".to_owned(),
        id: "toolu_1".to_owned(),
        v: 1,
    }));
    assert_eq!(
        code(&refused(&resolve)).0,
        codes::PRECONDITION_ALREADY_RESOLVED
    );
}

fn ticket(kind: &str, entity: Option<&str>) -> Ticket {
    Ticket {
        reply: json!(1),
        id: V7.to_owned(),
        kind: kind.to_owned(),
        entity: entity.map(str::to_owned),
    }
}

fn finished(kind: &str, ack: Ack) -> Result<CommandResult, Rejection> {
    finish(
        &ticket(kind, None),
        "ses_1",
        Outcome::Ack(ack),
        None,
        None,
        "t",
    )
}

#[test]
fn refusals_map_to_the_registered_codes() {
    let changed = finished(
        types::TURN_STOP,
        Ack::Rejected {
            refusal: Refusal::StopTargetChanged {
                current: TurnId::new("turn_2"),
            },
            reason: "x".to_owned(),
        },
    )
    .unwrap();
    let CommandResult::Rejected(rejected) = changed else {
        panic!("not rejected")
    };
    assert_eq!(rejected.code, codes::ADMISSION_STOP_TARGET_CHANGED);
    assert_eq!(rejected.current, Some(json!({"turnId": "turn_2"})));
    let held = finished(
        types::INPUT_SEND,
        Ack::Rejected {
            refusal: Refusal::QueueChoiceRequired,
            reason: "x".to_owned(),
        },
    )
    .unwrap();
    let CommandResult::Rejected(rejected) = held else {
        panic!("not rejected")
    };
    assert_eq!(
        rejected.allowed,
        Some(vec!["keepQueue".to_owned(), "discardQueue".to_owned()])
    );
    // 确认框看到的扣住的队列已经变了（§15.5）：带上现在扣住的
    let changed = finished(
        types::INPUT_SEND,
        Ack::Rejected {
            refusal: Refusal::HeldQueueChanged {
                current: vec![rust_cli_kernel_api::ids::CommandId::new("c2")],
            },
            reason: "x".to_owned(),
        },
    )
    .unwrap();
    let CommandResult::Rejected(rejected) = changed else {
        panic!("not rejected")
    };
    assert_eq!(rejected.code, codes::PRECONDITION_HELD_QUEUE_CHANGED);
    assert_eq!(rejected.current, Some(json!({"heldQueueItemIds": ["c2"]})));
    // 执行时失败是 failed（JSON-RPC 错误），不是 rejected（02 §6）
    let failed = finished(
        types::SET_MODEL,
        Ack::Rejected {
            refusal: Refusal::Failed,
            reason: "disk".to_owned(),
        },
    )
    .unwrap_err();
    assert_eq!(failed.code, codes::INTERNAL_FAILED);
}

/// 重放（02 §6、§11.2 A）：`duplicate{original}`，original 按同一张表换算（停止的回执带 stillQueued / cancelled）。
#[test]
fn a_duplicate_carries_its_original_result() {
    let original = Ack::Accepted {
        result: rust_cli_kernel_api::session_api::CommandResult::Unchanged,
    };
    let duplicate = finished(
        types::TURN_STOP,
        Ack::Duplicate {
            original: Box::new(original),
        },
    )
    .unwrap();
    let CommandResult::Duplicate(duplicate) = duplicate else {
        panic!("not a duplicate")
    };
    assert_eq!(duplicate.original["kind"], "accepted");
    assert_eq!(duplicate.original["noop"], true);
    assert_eq!(
        duplicate.original["result"],
        json!({"stillQueued": [], "cancelled": []})
    );
}

/// 没有消息的会话的视图。
fn empty_view() -> rust_cli_kernel_api::session_api::SessionView {
    let meta = rust_cli_kernel_api::store::SessionMeta {
        id: rust_cli_kernel_api::ids::SessionId::new("ses_1"),
        kind: rust_cli_kernel_api::store::SessionKind::Interactive,
        parent: None,
        workspace: rust_cli_kernel_api::env::WorkspaceRef {
            environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
            root: rust_cli_kernel_api::env::EnvPath::new(ROOT),
        },
        title: String::new(),
        trace: rust_cli_kernel_api::ids::TraceId::new("ses_1"),
        created_at_ms: 1,
    };
    rust_cli_kernel_api::session_api::SessionView::new(
        meta,
        Vec::new(),
        rust_cli_kernel_api::testing::selection(),
        PermissionMode::Default,
    )
}

/// 一个有队列的会话：投影与它的 topic（`queue` 里是 `items`，按顺序）。
fn queued_session(
    items: Vec<rust_cli_kernel_api::session_api::QueuedInput>,
) -> (Projection, crate::publisher::Publisher) {
    let view = rust_cli_kernel_api::session_api::SessionView {
        queue: items,
        ..empty_view()
    };
    let origin = super::super::projection::Origin {
        live: true,
        draft: false,
        deleted: false,
        title_source: rust_cli_kernel_api::session_facts::TitleSource::Default,
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

fn item(id: &str, text: &str, compact: bool) -> rust_cli_kernel_api::session_api::QueuedInput {
    rust_cli_kernel_api::session_api::QueuedInput {
        compact,
        ..rust_cli_kernel_api::session_api::QueuedInput::new(
            CommandId::new(id),
            text.to_owned(),
            Delivery::Queue,
        )
    }
}

fn at(id: &str, v: u64) -> Option<Expect> {
    Some(Expect::Alt1(crate::generated::core_v1::ExpectAlt1 {
        col: QUEUE.to_owned(),
        id: id.to_owned(),
        v,
    }))
}

/// `queue.edit` / `queue.reorder`（protocol-v5.md §17.1、§23.2）：`queue.edit` 的 `v` 相符之后，把投影里与它对应的整项交给内核再比一次；
/// 压缩项不能改；空正文是空输入。`queue.reorder` 不比 `v`（13 Q24）：旧 `v` 照样交给内核；项或锚点不在是 notFound。
#[test]
fn queue_edits_hand_the_kernel_the_item_the_client_saw() {
    let (projection, publisher) = queued_session(vec![
        item("c2", "second", false),
        item("c3", "third", false),
        item("k1", "", true),
    ]);
    let v = |id: &str| publisher.version(QUEUE, id).unwrap();
    let run = |envelope: &CommandEnvelope| accept(envelope, Some(&projection), Some(&publisher));
    let mut edit = envelope(types::QUEUE_EDIT, json!({"newText": "edited"}));
    edit.expect = at("c2", v("c2"));
    let Ok(Accepted {
        op: Operation::Live(LiveOp::Kernel(command)),
        ..
    }) = run(&edit)
    else {
        panic!("not a kernel command");
    };
    assert_eq!(
        command,
        Command::EditQueued {
            command: CommandId::new("c2"),
            edit: Box::new(rust_cli_kernel_api::session_api::QueuedEdit::text("edited")),
            guard: rust_cli_kernel_api::session_api::EditGuard::Seen {
                expected: Box::new(item("c2", "second", false)),
            },
        }
    );
    // 版本不符：带上现在的值
    edit.expect = at("c2", v("c2") + 1);
    let Err(stale) = run(&edit) else {
        panic!("accepted");
    };
    let CommandResult::Rejected(stale) = *stale else {
        panic!("not rejected");
    };
    assert_eq!(stale.code, codes::PRECONDITION_VERSION_MISMATCH);
    assert_eq!(stale.current.unwrap()["data"]["text"], "second");
    // 压缩项不能改；空正文是空输入
    let mut compact = envelope(types::QUEUE_EDIT, json!({"newText": "x"}));
    compact.expect = at("k1", v("k1"));
    assert_eq!(
        code(&run(&compact).err().unwrap()).0,
        codes::ADMISSION_QUEUE_ITEM_NOT_EDITABLE
    );
    let mut empty = envelope(types::QUEUE_EDIT, json!({"newText": "  "}));
    empty.expect = at("c3", v("c3"));
    assert_eq!(
        code(&run(&empty).err().unwrap()).0,
        codes::PRECONDITION_INPUT_EMPTY
    );
    // 重排：锚点不在 → notFound（带字段）；在 → 内核的 MoveQueued
    let mut reorder = envelope(types::QUEUE_REORDER, json!({"beforeId": "c9"}));
    reorder.expect = at("c3", v("c3"));
    assert_eq!(
        code(&run(&reorder).err().unwrap()),
        (codes::PRECONDITION_NOT_FOUND, Some("beforeId"))
    );
    reorder.payload = json!({"beforeId": "c2"});
    // 拖动不比 v：带的 v 旧了（例如前一项出队之前看到的）照样交给内核
    reorder.expect = at("c3", v("c3") + 1);
    let Ok(Accepted {
        op: Operation::Live(LiveOp::Kernel(command)),
        ..
    }) = run(&reorder)
    else {
        panic!("not a kernel command");
    };
    assert_eq!(
        command,
        Command::MoveQueued {
            command: CommandId::new("c3"),
            before: Some(CommandId::new("c2")),
        }
    );
    // 项不在队列里：notFound（不带字段）
    let mut gone = reorder.clone();
    gone.expect = at("c9", 1);
    assert_eq!(
        code(&run(&gone).err().unwrap()),
        (codes::PRECONDITION_NOT_FOUND, None)
    );
    reorder.payload = json!({"beforeId": null});
    assert!(run(&reorder).is_ok());
    // beforeId 是必填的（可以是 null）
    reorder.payload = json!({});
    assert_eq!(
        code(&run(&reorder).err().unwrap()).0,
        codes::PRECONDITION_INVALID_PAYLOAD
    );
}

/// 内核判出「变了」（两条命令背靠背、与出队并发，§17.4）：versionMismatch 带目标实体回执时的值。
#[test]
fn a_stale_refusal_carries_the_item_as_it_is_now() {
    let (projection, publisher) = queued_session(vec![item("c2", "edited", false)]);
    let result = finish(
        &ticket(types::QUEUE_EDIT, Some("c2")),
        "ses_1",
        Outcome::Ack(Ack::Rejected {
            refusal: Refusal::Stale,
            reason: "changed".to_owned(),
        }),
        Some(&projection),
        Some(&publisher),
        "t",
    )
    .unwrap();
    let CommandResult::Rejected(rejected) = result else {
        panic!("not rejected");
    };
    assert_eq!(rejected.code, codes::PRECONDITION_VERSION_MISMATCH);
    let current = rejected.current.unwrap();
    assert_eq!(current["v"], json!(publisher.version(QUEUE, "c2").unwrap()));
    assert_eq!(current["data"]["id"], "c2");
    assert_eq!(current["data"]["text"], "edited");
}

/// `session.compact`（protocol-v5.md §17.3、§23.2）：交给内核的 `CompactOrQueue`（排不排队、有没有压缩都由内核判），带这一次提交的模型；
/// 草稿 → admission.draft；参数严格。
#[test]
fn compact_goes_to_the_kernel_unless_the_session_is_a_draft() {
    let compact = envelope(types::SESSION_COMPACT, json!({}));
    let (projection, publisher) = queued_session(Vec::new());
    let Ok(Accepted {
        op: Operation::Live(LiveOp::Kernel(command)),
        ..
    }) = accept(&compact, Some(&projection), Some(&publisher))
    else {
        panic!("not a kernel command");
    };
    assert_eq!(command, Command::CompactOrQueue { selection: None });
    let chosen = envelope(
        types::SESSION_COMPACT,
        json!({"modelSelection": {"providerId": "p", "modelId": "m", "options": {"reasoningLevel": "high"}}}),
    );
    let Ok(Accepted {
        op: Operation::Live(LiveOp::Kernel(command)),
        ..
    }) = accept(&chosen, Some(&projection), Some(&publisher))
    else {
        panic!("not a kernel command");
    };
    let mut selection =
        rust_cli_kernel_api::model::ModelSelection::new("p".to_owned(), "m".to_owned());
    selection.reasoning = Some("high".to_owned());
    assert_eq!(
        command,
        Command::CompactOrQueue {
            selection: Some(Box::new(selection))
        }
    );
    let draft = super::super::projection::Origin {
        live: true,
        draft: true,
        deleted: false,
        title_source: rust_cli_kernel_api::session_facts::TitleSource::Default,
        mcp: Value::Null,
        subagents: Vec::new(),
        stamp: None,
        ..Default::default()
    };
    let (projection, _) = Projection::build(&empty_view(), draft);
    assert_eq!(
        code(&accept(&compact, Some(&projection), None).err().unwrap()).0,
        codes::ADMISSION_DRAFT
    );
    let extra = envelope(types::SESSION_COMPACT, json!({"instructions": "x"}));
    assert_eq!(
        code(&refused(&extra)),
        (codes::PRECONDITION_INVALID_PAYLOAD, Some("instructions"))
    );
}

/// `input.send` 的三种投递（protocol-v5.md §22.0）：没给处置按会话的跟随方式，`enqueue` 排队，`startNow` 抢占。
#[test]
fn each_disposition_is_its_own_kernel_command() {
    let command = |payload: Value| {
        let Ok(Accepted {
            op: Operation::Live(LiveOp::Kernel(command)),
            ..
        }) = accept(&envelope(types::INPUT_SEND, payload), None, None)
        else {
            panic!("not a kernel command");
        };
        command
    };
    assert!(matches!(
        command(json!({"text": "hi"})),
        Command::SubmitFollowUp { .. }
    ));
    assert!(matches!(
        command(json!({"text": "hi", "disposition": "enqueue"})),
        Command::Submit {
            delivery: Delivery::Queue,
            target: None,
            ..
        }
    ));
    assert!(matches!(
        command(json!({"text": "hi", "disposition": "startNow"})),
        Command::SubmitNow { .. }
    ));
    assert!(matches!(
        command(json!({"text": "hi", "disposition": "guide", "turnId": "turn_1"})),
        Command::Submit {
            delivery: Delivery::Steer,
            target: Some(_),
            ..
        }
    ));
}

/// `config.setFollowupMode`（K-S4）：交给通道按视图比较（值没变是 noop），换成内核的投递方式。
#[test]
fn the_follow_up_mode_reaches_the_lane() {
    for (mode, delivery) in [("guide", Delivery::Steer), ("queue", Delivery::Queue)] {
        let send = envelope(types::SET_FOLLOWUP_MODE, json!({"followupMode": mode}));
        let Ok(Accepted {
            op: Operation::Live(LiveOp::SetFollowUp(given)),
            ..
        }) = accept(&send, None, None)
        else {
            panic!("{mode}: not a follow-up change");
        };
        assert_eq!(given, delivery);
    }
    let unknown = envelope(types::SET_FOLLOWUP_MODE, json!({"followupMode": 3}));
    assert_eq!(
        code(&refused(&unknown)).0,
        codes::PRECONDITION_INVALID_PAYLOAD
    );
}

/// `work.cancel`（protocol-v5.md §20.3）：`expect` 指向 works 里在跑的一项 → 内核的 `StopTask`；不在了是 notFound，版本不符是
/// versionMismatch。
#[test]
fn a_work_cancel_stops_the_task_it_names() {
    let view = rust_cli_kernel_api::session_api::SessionView {
        works: vec![rust_cli_kernel_api::background::BackgroundStart {
            id: "b1".to_owned(),
            kind: rust_cli_kernel_api::manage::BackgroundKind::Shell,
            description: "npm test".to_owned(),
            tool_use: None,
            child: None,
            started_at: 1,
        }],
        ..empty_view()
    };
    let origin = super::super::projection::Origin {
        live: true,
        draft: false,
        deleted: false,
        title_source: rust_cli_kernel_api::session_facts::TitleSource::Default,
        mcp: Value::Null,
        subagents: Vec::new(),
        stamp: None,
        ..Default::default()
    };
    let (projection, content) = Projection::build(&view, origin);
    let mut publisher = crate::publisher::Publisher::new("ws/a/session/ses_1", "e");
    publisher.adopt(content);
    let v = publisher.version("works", "b1").unwrap();
    let cancel = |id: &str, v: u64| {
        let mut cancel = envelope("work.cancel", json!({}));
        cancel.expect = Some(Expect::Alt1(crate::generated::core_v1::ExpectAlt1 {
            col: "works".to_owned(),
            id: id.to_owned(),
            v,
        }));
        cancel
    };
    let accepted = accept(&cancel("b1", v), Some(&projection), Some(&publisher))
        .ok()
        .unwrap();
    let Operation::Live(LiveOp::Kernel(Command::StopTask { task })) = accepted.op else {
        panic!("not a stop");
    };
    assert_eq!(task, "b1");
    let refused_with =
        |envelope: CommandEnvelope| match accept(&envelope, Some(&projection), Some(&publisher)) {
            Err(result) => result,
            Ok(_) => panic!("accepted"),
        };
    assert_eq!(
        code(&refused_with(cancel("b9", v))).0,
        codes::PRECONDITION_NOT_FOUND
    );
    assert_eq!(
        code(&refused_with(cancel("b1", v + 1))).0,
        codes::PRECONDITION_VERSION_MISMATCH
    );
    let mut bare = envelope("work.cancel", json!({}));
    bare.expect = None;
    assert_eq!(
        code(&refused_with(bare)),
        (codes::PRECONDITION_INVALID_PAYLOAD, Some("expect"))
    );
}

/// 建会话带的附加目录（protocol-v5.md §32）：`executionDirectories` 原样交给通道建会话（相对路径、在工作区根里的由内核去掉），
/// 没有时为空。以前接受后丢弃（§15.12 第 5 项）。
#[test]
fn session_create_hands_its_execution_directories_to_the_lane() {
    let directories = |payload: serde_json::Value| match accept(
        &envelope(types::SESSION_CREATE, payload),
        None,
        None,
    )
    .unwrap()
    .op
    {
        Operation::Create { directories, .. } => directories,
        _ => panic!("not a create"),
    };
    assert_eq!(
        directories(
            json!({"sessionId": "ses_2", "executionDirectories": ["/Users/dev/other", "rel"]})
        ),
        ["/Users/dev/other", "rel"].map(rust_cli_kernel_api::env::EnvPath::new)
    );
    assert!(directories(json!({"sessionId": "ses_2"})).is_empty());
}

/// 新会话的归属（02 §11.9 第 1 项）：所属 Host 消费、转发之前去掉；直接发来的照常建会话，rust 不读它。
/// 参数仍严格：归属里多余的键拒绝，带字段路径。
#[test]
fn session_create_takes_the_hosts_ownership_and_ignores_it() {
    for ownership in [json!({"projectId": "p1"}), json!({"projectId": null})] {
        let create = envelope(
            types::SESSION_CREATE,
            json!({"sessionId": "ses_2", "ownership": ownership, "executionDirectories": [ROOT]}),
        );
        let accepted = accept(&create, None, None).unwrap();
        assert_eq!(accepted.sid, "ses_2");
        assert!(matches!(
            accepted.op,
            Operation::Create {
                persistence: Persistence::Deferred,
                ..
            }
        ));
    }
    let extra = envelope(
        types::SESSION_CREATE,
        json!({"sessionId": "ses_2", "ownership": {"projectId": "p1", "groupId": "g1"}}),
    );
    assert_eq!(
        code(&refused(&extra)),
        (
            codes::PRECONDITION_INVALID_PAYLOAD,
            Some("ownership.groupId")
        )
    );
}

/// 编辑锁与整条替换（protocol-v5.md §64.1）：`queue.lockEdit` / `queue.unlockEdit` 不比 `v`、由内核判；带 `editorId` 的 `queue.edit` 前置条件是锁
/// （旧的 `v` 照样交给内核），给了的字段换成内核的事实，没给的不带；`queue.applyConfig` 只取模型与模式（plan 不动）。
#[test]
fn edit_locks_and_whole_replacements_go_to_the_kernel() {
    let (projection, publisher) =
        queued_session(vec![item("c2", "second", false), item("k1", "", true)]);
    let v = |id: &str| publisher.version(QUEUE, id).unwrap();
    let run = |envelope: &CommandEnvelope| accept(envelope, Some(&projection), Some(&publisher));
    let kernel = |envelope: &CommandEnvelope| match run(envelope) {
        Ok(Accepted {
            op: Operation::Live(LiveOp::Kernel(command)),
            ..
        }) => command,
        _ => panic!("not a kernel command"),
    };
    let mut lock = envelope(types::QUEUE_LOCK_EDIT, json!({"editorId": "e1"}));
    lock.expect = at("c2", v("c2") + 5);
    assert_eq!(
        kernel(&lock),
        Command::LockQueued {
            command: CommandId::new("c2"),
            editor: "e1".to_owned(),
        }
    );
    let mut unlock = envelope(types::QUEUE_UNLOCK_EDIT, json!({}));
    unlock.expect = at("c9", 1);
    assert_eq!(
        kernel(&unlock),
        Command::UnlockQueued {
            command: CommandId::new("c9"),
        }
    );
    let mut replace = envelope(
        types::QUEUE_EDIT,
        json!({
            "newText": "edited",
            "editorId": "e1",
            "attachments": [],
            "quotes": [{"text": "q"}],
            "modelSelection": {"providerId": "p", "modelId": "m"},
            "mode": "edit",
            "planEnabled": true,
        }),
    );
    replace.expect = at("c2", v("c2") + 5);
    let Command::EditQueued {
        command,
        edit,
        guard,
    } = kernel(&replace)
    else {
        panic!("not an edit");
    };
    assert_eq!(command, CommandId::new("c2"));
    assert_eq!(
        guard,
        rust_cli_kernel_api::session_api::EditGuard::Lock {
            editor: "e1".to_owned()
        }
    );
    assert_eq!(edit.text, "edited");
    assert_eq!(edit.attachments, Some(Default::default()));
    assert_eq!(edit.quotes.as_ref().map(Vec::len), Some(1));
    assert_eq!(edit.config.mode, Some(PermissionMode::AcceptEdits));
    assert_eq!(edit.config.plan, Some(true));
    assert_eq!(
        edit.config.selection.as_deref().map(|s| s.model.as_str()),
        Some("m")
    );
    // 没给的字段不带（内核保留排着的那一份）
    let mut text_only = envelope(
        types::QUEUE_EDIT,
        json!({"newText": "edited", "editorId": "e1"}),
    );
    text_only.expect = at("c2", v("c2"));
    let Command::EditQueued { edit, .. } = kernel(&text_only) else {
        panic!("not an edit");
    };
    assert_eq!(edit.attachments, None);
    assert_eq!(edit.quotes, None);
    assert!(edit.config.is_unset());
    // 压缩项不能改，带锁也一样
    let mut compact = envelope(types::QUEUE_EDIT, json!({"newText": "x", "editorId": "e1"}));
    compact.expect = at("k1", v("k1"));
    assert_eq!(
        code(&run(&compact).err().unwrap()).0,
        codes::ADMISSION_QUEUE_ITEM_NOT_EDITABLE
    );
    let apply = envelope(
        types::QUEUE_APPLY_CONFIG,
        json!({"queueItemIds": ["c2", "c9"], "mode": "guarded"}),
    );
    assert_eq!(
        kernel(&apply),
        Command::ApplyQueuedConfig {
            commands: vec![CommandId::new("c2"), CommandId::new("c9")],
            config: rust_cli_kernel_api::input_extras::TurnConfig {
                selection: None,
                mode: Some(PermissionMode::Guarded),
                plan: None,
            },
        }
    );
    let bad = envelope(
        types::QUEUE_APPLY_CONFIG,
        json!({"queueItemIds": ["c2"], "mode": "turbo"}),
    );
    assert_eq!(
        code(&run(&bad).err().unwrap()).0,
        codes::PRECONDITION_INVALID_MODE
    );
}

/// 锁的拒绝与 `Unchanged`（§64.1）：`QueueItemLocked` → `admission.queueItemEditLocked`；`EditLockLost` → `precondition.queueEditLockLost`；
/// 锁不住的项（`Unavailable`）→ `admission.queueItemNotEditable`；`Unchanged` → `accepted{noop}`。
#[test]
fn lock_refusals_map_to_their_codes() {
    let (projection, publisher) = queued_session(vec![item("c2", "second", false)]);
    let result = |kind: &str, ack: Ack| {
        finish(
            &ticket(kind, Some("c2")),
            "ses_1",
            Outcome::Ack(ack),
            Some(&projection),
            Some(&publisher),
            "t",
        )
        .unwrap()
    };
    let rejected = |refusal: Refusal| Ack::Rejected {
        refusal,
        reason: "r".to_owned(),
    };
    for (kind, refusal, expected) in [
        (
            types::QUEUE_LOCK_EDIT,
            Refusal::QueueItemLocked,
            codes::ADMISSION_QUEUE_ITEM_EDIT_LOCKED,
        ),
        (
            types::QUEUE_EDIT,
            Refusal::EditLockLost,
            codes::PRECONDITION_QUEUE_EDIT_LOCK_LOST,
        ),
        (
            types::QUEUE_LOCK_EDIT,
            Refusal::Unavailable,
            codes::ADMISSION_QUEUE_ITEM_NOT_EDITABLE,
        ),
    ] {
        let CommandResult::Rejected(rejected) = result(kind, rejected(refusal)) else {
            panic!("not rejected");
        };
        assert_eq!(rejected.code, expected);
    }
    let CommandResult::Accepted(noop) = result(
        types::QUEUE_UNLOCK_EDIT,
        Ack::Accepted {
            result: rust_cli_kernel_api::session_api::CommandResult::Unchanged,
        },
    ) else {
        panic!("not accepted");
    };
    assert!(noop.noop);
}
