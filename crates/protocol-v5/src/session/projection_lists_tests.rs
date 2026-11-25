//! 队列的顺序与失败的压缩标记（protocol-v5.md §23.1，02 §11.8）：别的项出队、挪动时这一项的 `v` 不变，顺序在 `state.queueOrder`；
//! 压缩的回合以失败结束时活会话与冷读出同一行失败的标记。排队项带接受时的附件与引用（§15.3.2，从 projection_tests.rs 移来：800 行上限）。
//! 由视图建的投影带视图里还没有结局的输入（02 §1.5，§56.2）。

use rust_cli_kernel_api::ids::{CommandId, MessageId, TurnId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::session_api::{
    Delivery, InputEnd, InputFate, Joined, KernelEvent, QueuedInput, Running, SequencedEvent,
    TurnOrigin, TurnOutcome, TurnStart,
};
use rust_cli_kernel_api::store::TurnRecord;
use rust_cli_kernel_api::user_command::LOCAL_COMMAND_OUTPUT_SOURCE;
use serde_json::{Value, json};

use super::super::tests::{attached, live, origin, snapshot, view};
use super::*;

fn queued(id: &str) -> QueuedInput {
    QueuedInput::new(CommandId::new(id), id.to_owned(), Delivery::Queue)
}

fn changed(ids: &[&str]) -> KernelEvent {
    KernelEvent::QueueChanged {
        items: ids.iter().map(|id| queued(id)).collect(),
    }
}

/// 发布一个事件（seq 接着投影已应用的往下编）。
fn apply(projection: &mut Projection, publisher: &mut Publisher, event: KernelEvent) {
    let event = SequencedEvent {
        seq: projection.applied + 1,
        event,
        revision: 0,
    };
    let batch = projection.apply(publisher, &event);
    publisher.publish(batch);
}

/// 13 Q24：前一项出队、挪动别的项，这一项不再 put、`v` 不变；顺序跟着 `state.queueOrder` 变。以前 `order.queuePosition` 在项上，
/// 前一项出队时后面每一项都 put、`v` 都变。
#[test]
fn a_dequeue_or_a_move_leaves_the_other_items_and_their_versions() {
    let (mut projection, mut publisher) = live(vec![changed(&["c2", "c3", "c4"])]);
    let v3 = publisher.version(QUEUE, "c3").unwrap();
    let v4 = publisher.version(QUEUE, "c4").unwrap();
    assert_eq!(
        publisher.entity(QUEUE, "c3").unwrap()["order"],
        json!({"admissionSeq": 2})
    );
    assert_eq!(
        snapshot(&publisher)["state"]["queueOrder"],
        json!(["c2", "c3", "c4"])
    );
    apply(&mut projection, &mut publisher, changed(&["c3", "c4"]));
    apply(&mut projection, &mut publisher, changed(&["c4", "c3"]));
    assert!(publisher.entity(QUEUE, "c2").is_none());
    assert_eq!(publisher.version(QUEUE, "c3"), Some(v3));
    assert_eq!(publisher.version(QUEUE, "c4"), Some(v4));
    assert_eq!(
        snapshot(&publisher)["state"]["queueOrder"],
        json!(["c4", "c3"])
    );
}

/// 失败的压缩标记的字段（不含版本与行号：活会话在不同的批里建，冷读一批建出）。
fn failed_marker(publisher: &Publisher) -> Value {
    let mut row = json!(
        publisher
            .entity(rows::ROWS, "turn_compact:compact")
            .unwrap()
    );
    row.as_object_mut().unwrap().remove(rows::CREATED_AT_SEQ);
    row
}

/// 02 §11.8 第 5 项：压缩的回合以失败结束 → 一行失败的压缩标记（回合 id 加后缀），活会话（回合结束的事件）与冷读（回合记录的来源与
/// 结局）同一行；`lastError` 照旧。成功、中断的压缩回合与失败的普通回合都不出这一行。
#[test]
fn a_failed_compaction_has_the_same_failed_marker_live_and_cold() {
    let turn = TurnId::new("turn_compact");
    let started_at = 1_700;
    let (_, live_publisher) = live(vec![
        KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: None,
            origin: TurnOrigin::Compaction,
            started_at,
            works: Vec::new(),
        },
        KernelEvent::TurnFinished {
            turn: turn.clone(),
            outcome: TurnOutcome::Failed {
                message: "Not enough messages to compact.".to_owned(),
                cause: None,
            },
            usage: None,
            time: super::super::kit::ENDED,
            files: Vec::new(),
        },
    ]);
    let row = failed_marker(&live_publisher);
    assert_eq!(row["kind"], "timelineMarker");
    assert_eq!(row["marker"], "compact");
    assert_eq!(
        row["detail"],
        json!({"type": "compact", "origin": "manual", "status": "failed"})
    );
    assert_eq!(row["turnId"], "turn_compact");
    assert_eq!(row["createdAt"], rows::millis(started_at));
    assert_eq!(
        snapshot(&live_publisher)["state"]["control"]["lastError"]["message"],
        "Not enough messages to compact."
    );
    // 冷读：失败时提交的只有本地命令的记录（meta，不成行）；回合记录带着来源与结局
    let record = Message::User(UserMessage {
        id: MessageId::new("msg_record"),
        content: vec![ContentBlock::Text {
            text: "stderr".to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: LOCAL_COMMAND_OUTPUT_SOURCE.to_owned(),
        },
    });
    let ended = |outcome| TurnRecord {
        turn: turn.clone(),
        first: MessageId::new("msg_record"),
        origin: TurnOrigin::Compaction,
        // 这条测的是失败标记的形状，与开回合的命令无关；没有命令的记录照样出标记（ROW-SOURCE 之前写的记录就是这样）
        command: None,
        started_at,
        ended: super::super::kit::ended(outcome, Vec::new()),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let cold = |record_of: TurnRecord| {
        let (_, content) =
            Projection::build(&view(vec![record.clone()], vec![record_of]), origin(false));
        let mut publisher = Publisher::new("ws/a/session/ses_1", "e");
        publisher.adopt(content);
        publisher
    };
    assert_eq!(failed_marker(&cold(ended(InputEnd::Failed))), row);
    // 成功的压缩回合只有边界行；被停止的出 `cancelled`（protocol-v5.md §50.1，projection_compact_tests.rs）
    assert!(
        cold(ended(InputEnd::Completed))
            .entity(rows::ROWS, "turn_compact:compact")
            .is_none()
    );
    let input = TurnRecord {
        origin: TurnOrigin::Input,
        ..ended(InputEnd::Failed)
    };
    assert!(
        cold(input)
            .entity(rows::ROWS, "turn_compact:compact")
            .is_none()
    );
}

/// 一条输入的结局去掉版本（活会话与由视图建的在不同的批里写）。
fn fate_of(publisher: &Publisher, input: &str) -> Value {
    let mut fields = json!(publisher.entity(INPUTS, input).unwrap());
    fields.as_object_mut().unwrap().remove("v");
    fields
}

/// 02 §1.5：非终态（queued、started）一直在 `inputs` 里（protocol-v5.md §56.2）。由视图建的投影（改写历史、激活、物化）以前 `inputs` 为空：
/// 视图之前的结局 seq 不大于 `last_seq`、不再应用，改写历史之后的快照里没有在跑的那条输入与排着的输入。现在与活会话同一组输入、同样的
/// `fate` 与 `turnId`；进回合的时刻取回合开始的时刻，排进队列的时刻视图里没有。排队的压缩不是输入。
#[test]
fn a_view_gives_the_inputs_still_running_or_queued() {
    let turn = TurnId::new("turn_2");
    let start = TurnStart {
        turn: turn.clone(),
        command: Some(CommandId::new("c_edit")),
        origin: TurnOrigin::Input,
        started_at: 40,
        works: Vec::new(),
    };
    let compact = QueuedInput {
        compact: true,
        ..queued("c_compact")
    };
    let (_, live) = live(vec![
        KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: start.command.clone(),
            origin: TurnOrigin::Input,
            started_at: start.started_at,
            works: Vec::new(),
        },
        KernelEvent::InputFate {
            input: CommandId::new("c_edit"),
            fate: InputFate::Started {
                turn,
                joined: Joined::Opened,
            },
            at: start.started_at,
        },
        KernelEvent::QueueChanged {
            items: vec![queued("c_next"), compact.clone()],
        },
        KernelEvent::InputFate {
            input: CommandId::new("c_next"),
            fate: InputFate::Queued,
            at: 50,
        },
    ]);
    let mut view = view(Vec::new(), Vec::new());
    view.running = Some(Running::Turn(start));
    view.queue = vec![queued("c_next"), compact];
    let (_, content) = Projection::build(&view, origin(true));
    let mut built = super::super::kit::publisher();
    built.adopt(content);
    assert_eq!(fate_of(&built, "c_edit"), fate_of(&live, "c_edit"));
    assert_eq!(fate_of(&built, "c_edit")["fate"], "started");
    let (mut queued_built, mut queued_live) = (fate_of(&built, "c_next"), fate_of(&live, "c_next"));
    assert_eq!(queued_built["at"], 0);
    queued_built.as_object_mut().unwrap().remove("at");
    queued_live.as_object_mut().unwrap().remove("at");
    assert_eq!(queued_built, queued_live);
    assert!(built.entity(INPUTS, "c_compact").is_none());
    assert!(live.entity(INPUTS, "c_compact").is_none());
}

/// 在跑的是压缩回合（`session.compact` 开的）：它的命令不是输入，`inputs` 里没有它（02 §1.5，同活会话：内核不为它发结局）。
#[test]
fn a_running_compaction_is_not_an_input() {
    let mut view = view(Vec::new(), Vec::new());
    view.running = Some(Running::Turn(TurnStart {
        turn: TurnId::new("turn_c"),
        command: Some(CommandId::new("c_compact")),
        origin: TurnOrigin::Compaction,
        started_at: 40,
        works: Vec::new(),
    }));
    let (_, content) = Projection::build(&view, origin(true));
    let mut built = super::super::kit::publisher();
    built.adopt(content);
    assert!(
        snapshot(&built)["collections"]["inputs"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
}

/// 排队的输入带着接受时的附件与引用（§15.3.2）；没有引用时不写 `quotes`。
#[test]
fn a_queue_item_shows_its_attachments_and_quotes() {
    let mut item = queued("c2");
    item.extras.files = attached().files;
    item.extras.quotes = vec![rust_cli_kernel_api::input_extras::Quote {
        text: "q".to_owned(),
        path: Some("/Users/dev/a/notes.md".to_owned()),
        sender_name: None,
        sender_id: None,
        message_id: None,
        sent_at_ms: None,
    }];
    let (_, publisher) = live(vec![KernelEvent::QueueChanged {
        items: vec![item, queued("c3")],
    }]);
    let queue = snapshot(&publisher)["collections"]["queue"].clone();
    assert_eq!(queue[0]["attachments"][0]["ref"], "/Users/dev/a/notes.md");
    assert_eq!(
        queue[0]["quotes"],
        json!([{"text": "q", "path": "/Users/dev/a/notes.md"}])
    );
    assert!(queue[1].get("quotes").is_none());
}

/// 编辑锁（§64.1）：锁着的项带 `editLock{editorId}`，放开之后没有；空闲、队首锁着时 `admission.input` 是 enqueue（原因 `queueHeadEditLocked`），
/// 锁在后面的项上时照旧 startNow。
#[test]
fn a_locked_item_shows_its_editor_and_a_locked_head_queues_new_inputs() {
    let locked = |id: &str, editor: Option<&str>| QueuedInput {
        edit_lock: editor.map(|editor| rust_cli_kernel_api::session_api::QueueEditLock {
            editor: editor.to_owned(),
        }),
        ..queued(id)
    };
    let (mut projection, mut publisher) = live(vec![KernelEvent::QueueChanged {
        items: vec![locked("c2", Some("e1")), queued("c3")],
    }]);
    assert_eq!(
        publisher.entity(QUEUE, "c2").unwrap()["editLock"],
        json!({"editorId": "e1"})
    );
    assert!(
        publisher
            .entity(QUEUE, "c3")
            .unwrap()
            .get("editLock")
            .is_none()
    );
    let input = |publisher: &Publisher| snapshot(publisher)["state"]["admission"]["input"].clone();
    assert_eq!(input(&publisher)["mode"], "enqueue");
    assert_eq!(
        input(&publisher)["reason"],
        codes::ADMISSION_QUEUE_HEAD_EDIT_LOCKED
    );
    apply(
        &mut projection,
        &mut publisher,
        KernelEvent::QueueChanged {
            items: vec![queued("c2"), locked("c3", Some("e1"))],
        },
    );
    assert!(
        publisher
            .entity(QUEUE, "c2")
            .unwrap()
            .get("editLock")
            .is_none()
    );
    assert_eq!(input(&publisher)["mode"], "startNow");
}
