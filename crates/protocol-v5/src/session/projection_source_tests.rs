//! 行带上产生它的命令（protocol-v5.md §24）：用户输入行、插话行、回合头、压缩标记的 `sourceCommandId`，活会话与冷读同值。

use rust_cli_kernel_api::ids::{CommandId, MessageId, TurnId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::session_api::{KernelEvent, SessionView, TurnOrigin};
use rust_cli_kernel_api::store::TurnRecord;
use serde_json::{Value, json};

use super::*;

fn source(message: &str, command: &str) -> rust_cli_kernel_api::store::InputSource {
    rust_cli_kernel_api::store::InputSource {
        message: MessageId::new(message),
        command: CommandId::new(command),
    }
}

/// 工具边界并入的插话（`queued_command` attachment 的 prompt 模式）。
fn steered(id: &str, text: &str) -> Message {
    use rust_cli_kernel_api::hooks::queued_command;
    Message::Attachment(rust_cli_kernel_api::message::AttachmentMessage {
        id: MessageId::new(id),
        source: queued_command::SOURCE.to_owned(),
        text: text.to_owned(),
        data: json!({ queued_command::PROMPT_KEY: text, queued_command::MODE_KEY: queued_command::PROMPT_MODE }),
    })
}

/// 行带上产生它的命令（protocol-v5.md §24.1）：开回合的用户行、并入的插话行各是自己输入的命令，回合头是开回合的命令；
/// 活会话（`InputCommitted` 在它的 `MessageDone` 之前）与冷读（视图的 `sources`、回合记录的 `command`）同值；回答行不带。
/// 以前三种行都不写，界面的发送漏斗等不到「这条命令的行画出来了」，30 s 后记成失败。
#[test]
fn rows_carry_the_command_that_produced_them_live_and_cold() {
    let (mut messages, mut events) = conversation();
    events.insert(
        1,
        KernelEvent::InputCommitted {
            source: source("msg_u", "c1"),
        },
    );
    // 插话在工具结果之后并入（在最后的回答之前）
    let at = events
        .iter()
        .position(|event| matches!(event, KernelEvent::MessageDone { message } if message.id().as_str() == "msg_r"))
        .unwrap();
    events.insert(
        at + 1,
        KernelEvent::InputCommitted {
            source: source("msg_q", "c2"),
        },
    );
    events.insert(
        at + 2,
        KernelEvent::MessageDone {
            message: steered("msg_q", "also b"),
        },
    );
    messages.insert(3, steered("msg_q", "also b"));
    let (_, publisher) = live(events);
    let cold_view = SessionView {
        sources: vec![source("msg_u", "c1"), source("msg_q", "c2")],
        tool_times: vec![tool_time()],
        ..view(messages, vec![record()])
    };
    let (_, content) = Projection::build(&cold_view, origin(false));
    let mut cold = Publisher::new("t", "e").with_window(
        rows::ROWS,
        rows::CREATED_AT_SEQ,
        SESSION_SNAPSHOT_ROWS,
        usize::MAX,
    );
    cold.adopt(content);
    let rows = contents(&rows_of(&publisher));
    assert_eq!(rows, contents(&rows_of(&cold)));
    let command =
        |id: &str| rows.iter().find(|row| row["id"] == id).unwrap()["sourceCommandId"].clone();
    assert_eq!(command(TURN), "c1");
    assert_eq!(command("msg_u"), "c1");
    assert_eq!(command("msg_q"), "c2");
    assert_eq!(
        command("msg_a"),
        Value::Null,
        "an answer row has no command"
    );
    // 没有关联的输入（本功能之前写的会话）：不写这个字段，不猜
    let (_, content) = Projection::build(&view(conversation().0, Vec::new()), origin(false));
    let mut bare = Publisher::new("t", "e");
    bare.adopt(content);
    assert!(
        bare.entity(rows::ROWS, "msg_u")
            .unwrap()
            .get("sourceCommandId")
            .is_none()
    );
}

/// 压缩标记带它所在压缩回合的命令（protocol-v5.md §24.1，`session.compact` 的 commandId），活会话与冷读同值；输入回合里的自动压缩不带（同 v4）。
/// 以前不写，界面认不出「这次压缩是哪条命令的」。
#[test]
fn a_compaction_marker_carries_the_compact_command() {
    let trigger = rust_cli_kernel_api::message::CompactTrigger::Manual;
    let (_, publisher) = live(vec![
        compaction_started(),
        KernelEvent::MessageDone {
            message: boundary(trigger),
        },
    ]);
    assert_eq!(
        publisher.entity(rows::ROWS, "msg_bd").unwrap()["sourceCommandId"],
        "k1"
    );
    assert_eq!(
        publisher.entity(rows::ROWS, COMPACTION).unwrap()["sourceCommandId"],
        "k1"
    );
    let record = TurnRecord {
        turn: TurnId::new(COMPACTION),
        first: MessageId::new("msg_bd"),
        origin: TurnOrigin::Compaction,
        started_at: 1,
        command: Some(CommandId::new("k1")),
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let (_, content) =
        Projection::build(&view(vec![boundary(trigger)], vec![record]), origin(false));
    let mut cold = Publisher::new("t", "e");
    cold.adopt(content);
    assert_eq!(
        cold.entity(rows::ROWS, "msg_bd").unwrap()["sourceCommandId"],
        "k1"
    );
    let (_, auto) = live(vec![
        KernelEvent::TurnStarted {
            turn: TurnId::new(TURN),
            command: Some(CommandId::new("c1")),
            origin: TurnOrigin::Input,
            started_at: 1,
            works: Vec::new(),
        },
        KernelEvent::MessageDone {
            message: boundary(rust_cli_kernel_api::message::CompactTrigger::Auto),
        },
    ]);
    assert!(
        auto.entity(rows::ROWS, "msg_bd")
            .unwrap()
            .get("sourceCommandId")
            .is_none()
    );
}

/// 在跑的压缩回合的开始（视图的 `running`）。
fn compaction_start() -> rust_cli_kernel_api::session_api::TurnStart {
    rust_cli_kernel_api::session_api::TurnStart {
        turn: TurnId::new(COMPACTION),
        command: Some(CommandId::new("k1")),
        origin: TurnOrigin::Compaction,
        started_at: 5,
        works: Vec::new(),
    }
}

/// 由视图建投影（活会话）。
fn built(view: &SessionView) -> (Projection, Publisher) {
    let (projection, content) = Projection::build(view, origin(true));
    let mut publisher = Publisher::new("t", "e").with_window(
        rows::ROWS,
        rows::CREATED_AT_SEQ,
        SESSION_SNAPSHOT_ROWS,
        usize::MAX,
    );
    publisher.adopt(content);
    (projection, publisher)
}

fn publish(projection: &mut Projection, publisher: &mut Publisher, seq: u64, event: KernelEvent) {
    let batch = projection.apply(
        publisher,
        &rust_cli_kernel_api::session_api::SequencedEvent {
            seq,
            event,
            revision: 0,
        },
    );
    publisher.publish(batch);
}

/// 修 bug（protocol-v5.md §25.1，session-lifecycle.md §10.33）：回合中途由视图建的投影，在跑的是压缩回合、它的边界已经落库（回合记录在）时，
/// 投影要知道它是压缩、何时开始。以前视图只给回合 id：`control.activeWorks` 读成普通回合、没有开始时刻，`admission.compact` 放开，
/// 压缩失败时没有失败的标记。
#[test]
fn a_projection_built_mid_compaction_knows_the_running_turn() {
    let record = TurnRecord {
        turn: TurnId::new(COMPACTION),
        first: MessageId::new("msg_bd"),
        origin: TurnOrigin::Compaction,
        started_at: 5,
        command: Some(CommandId::new("k1")),
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let mid = SessionView {
        running: Some(rust_cli_kernel_api::session_api::Running::Turn(
            compaction_start(),
        )),
        ..view(
            vec![boundary(
                rust_cli_kernel_api::message::CompactTrigger::Manual,
            )],
            vec![record],
        )
    };
    let (mut projection, mut publisher) = built(&mid);
    let state = snapshot(&publisher)["state"].clone();
    assert_eq!(
        state["control"]["activeWorks"],
        json!([{"id": COMPACTION, "kind": "compact", "startedAtSeq": 0, "startedAt": 5.0}])
    );
    assert_eq!(
        state["admission"]["compact"],
        json!({"allow": false, "reason": "admission.compactLocked"})
    );
    publish(
        &mut projection,
        &mut publisher,
        1,
        KernelEvent::TurnFinished {
            turn: TurnId::new(COMPACTION),
            outcome: TurnOutcome::Failed {
                message: "boom".to_owned(),
                cause: None,
            },
            usage: None,
            time: super::super::kit::ENDED,
            files: Vec::new(),
        },
    );
    assert!(
        publisher
            .entity(rows::ROWS, &rows::turn_compaction_id(COMPACTION))
            .is_some(),
        "a failed compaction leaves its marker"
    );
}

/// 修 bug（protocol-v5.md §24.5 第 1 项、§25.1）：压缩回合的边界还没落库（没有回合记录）时由视图建的投影，补建的回合头带来源、
/// 压缩的命令与开始时刻，之后到的压缩标记带压缩的命令——与活会话由事件建的同值。以前回合头是 `origin: unknown`、没有命令与开始时刻，
/// 标记也没有命令。
#[test]
fn a_projection_built_before_the_compaction_boundary_gives_the_same_rows() {
    let trigger = rust_cli_kernel_api::message::CompactTrigger::Manual;
    let mid = SessionView {
        running: Some(rust_cli_kernel_api::session_api::Running::Turn(
            compaction_start(),
        )),
        ..view(Vec::new(), Vec::new())
    };
    let (mut projection, mut publisher) = built(&mid);
    publish(
        &mut projection,
        &mut publisher,
        1,
        KernelEvent::MessageDone {
            message: boundary(trigger),
        },
    );
    let (_, live) = live(vec![
        KernelEvent::TurnStarted {
            turn: TurnId::new(COMPACTION),
            command: Some(CommandId::new("k1")),
            origin: TurnOrigin::Compaction,
            started_at: 5,
            works: Vec::new(),
        },
        KernelEvent::MessageDone {
            message: boundary(trigger),
        },
    ]);
    let header = publisher.entity(rows::ROWS, COMPACTION).unwrap();
    assert_eq!(header["origin"], "synthetic");
    assert_eq!(header["sourceCommandId"], "k1");
    assert_eq!(
        publisher.entity(rows::ROWS, "msg_bd").unwrap()["sourceCommandId"],
        "k1"
    );
    assert_eq!(contents(&rows_of(&publisher)), contents(&rows_of(&live)));
}
