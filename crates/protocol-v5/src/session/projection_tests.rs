//! 会话投影（protocol-v5.md §15.3、§15.11）：冷读与活会话同一组行；流式增量对到最终的行；回合结束的收尾；窗口与
//! `rowsWindow`；队列整份来时的增删；输入结局的换算与保留；交互 id = tool_use；准入的换算。

use rust_cli_kernel_api::ids::{
    CommandId, InteractionId, MessageId, SessionId, ToolUseId, TraceId, TurnId,
};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::BlockDelta;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Delivery, DropReason, InputEnd, InputFate, Joined, KernelEvent, QueueHold, QueuedInput,
    SequencedEvent, SessionView, TurnOrigin, TurnOutcome,
};
use rust_cli_kernel_api::session_facts::TitleSource;
use rust_cli_kernel_api::store::{SessionKind, SessionMeta, TurnRecord};
use rust_cli_kernel_api::tool::ToolOutput;
use serde_json::{Value, json};

use super::*;
use crate::consts::{INPUT_FATE_RETAIN, SESSION_SNAPSHOT_ROWS};

const TURN: &str = "turn_1";

pub(super) fn view(messages: Vec<Message>, turns: Vec<TurnRecord>) -> SessionView {
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

pub(super) fn origin(live: bool) -> Origin {
    Origin {
        live,
        draft: false,
        deleted: false,
        title_source: TitleSource::Default,
        mcp: Value::Null,
        subagents: Vec::new(),
        stamp: None,
        ..Default::default()
    }
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

fn assistant(id: &str, content: Vec<ContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content,
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn results(id: &str, call: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(call),
            content: vec![ToolResultContent::Text {
                text: text.to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("msg_a"),
        },
    })
}

fn delta(message: &str, index: usize, tool_use: Option<&str>, delta: BlockDelta) -> KernelEvent {
    KernelEvent::StreamDelta {
        turn: TurnId::new(TURN),
        response: 0,
        message: MessageId::new(message),
        index,
        tool_use: tool_use.map(ToolUseId::new),
        delta,
    }
}

/// 这段对话里那次工具调用的起止：活会话的 `ToolStarted` / `ToolTimeRecorded` 与冷读视图的 `tool_times` 同一份（§31.1）。
fn tool_time() -> rust_cli_kernel_api::metering::ToolTime {
    rust_cli_kernel_api::metering::ToolTime {
        tool_use: ToolUseId::new("toolu_1"),
        started_at: 150,
        ended_at: 180,
    }
}

/// 同一段对话：一个回合里用户输入、思考 + 正文 + 一次工具调用、工具结果、最后的正文。
fn conversation() -> (Vec<Message>, Vec<KernelEvent>) {
    let first = assistant(
        "msg_a",
        vec![
            ContentBlock::Thinking {
                thinking: "think".to_owned(),
                signature: Some("sig".to_owned()),
            },
            ContentBlock::Text {
                text: "Hello".to_owned(),
            },
            ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_1"),
                name: "Read".to_owned(),
                input: json!({"a": 1}),
            },
        ],
    );
    let last = assistant(
        "msg_b",
        vec![ContentBlock::Text {
            text: "done".to_owned(),
        }],
    );
    let messages = vec![
        user("msg_u", "hi"),
        first.clone(),
        results("msg_r", "toolu_1", "ok"),
        last.clone(),
    ];
    let events = vec![
        KernelEvent::TurnStarted {
            turn: TurnId::new(TURN),
            command: Some(CommandId::new("c1")),
            origin: TurnOrigin::Input,
            started_at: 100,
            works: Vec::new(),
        },
        KernelEvent::MessageDone {
            message: user("msg_u", "hi"),
        },
        delta("msg_a", 0, None, BlockDelta::Thinking("think".to_owned())),
        delta("msg_a", 1, None, BlockDelta::Text("Hel".to_owned())),
        delta("msg_a", 1, None, BlockDelta::Text("lo".to_owned())),
        delta(
            "msg_a",
            2,
            Some("toolu_1"),
            BlockDelta::InputJson("{\"a\":".to_owned()),
        ),
        delta(
            "msg_a",
            2,
            Some("toolu_1"),
            BlockDelta::InputJson("1}".to_owned()),
        ),
        KernelEvent::ToolStarted {
            tool_use: ToolUseId::new("toolu_1"),
            name: "Read".to_owned(),
            input: json!({"a": 1}),
            started_at: tool_time().started_at,
        },
        KernelEvent::MessageDone { message: first },
        KernelEvent::ToolDone {
            tool_use: ToolUseId::new("toolu_1"),
            output: ToolOutput {
                content: vec![ToolResultContent::Text {
                    text: "ok".to_owned(),
                }],
                is_error: false,
                display: Value::Null,
            },
            cancelled: false,
        },
        KernelEvent::ToolTimeRecorded { time: tool_time() },
        KernelEvent::MessageDone {
            message: results("msg_r", "toolu_1", "ok"),
        },
        delta("msg_b", 0, None, BlockDelta::Text("done".to_owned())),
        KernelEvent::MessageDone { message: last },
        KernelEvent::TurnFinished {
            turn: TurnId::new(TURN),
            outcome: TurnOutcome::Success,
            usage: None,
            time: super::kit::ENDED,
            files: Vec::new(),
        },
    ];
    (messages, events)
}

/// 这段对话的回合记录：回合结束时再写的那一份（结局与结束的时刻、用时，与活会话的 `TurnFinished` 同值）。
fn record() -> TurnRecord {
    TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new("msg_u"),
        origin: TurnOrigin::Input,
        started_at: 100,
        command: Some(CommandId::new("c1")),
        ended: super::kit::ended(InputEnd::Completed, Vec::new()),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    }
}

/// 活会话：空视图起步，逐个事件发布。
pub(super) fn live(events: Vec<KernelEvent>) -> (Projection, Publisher) {
    let (mut projection, content) = Projection::build(&view(Vec::new(), Vec::new()), origin(true));
    let mut publisher = Publisher::new("ws/a/session/ses_1", "e").with_window(
        rows::ROWS,
        rows::CREATED_AT_SEQ,
        SESSION_SNAPSHOT_ROWS,
        usize::MAX,
    );
    publisher.adopt(content);
    for (n, event) in events.into_iter().enumerate() {
        let event = SequencedEvent {
            seq: n as u64 + 1,
            event,
            revision: 0,
        };
        let batch = projection.apply(&publisher, &event);
        publisher.publish(batch);
    }
    (projection, publisher)
}

fn rows_of(publisher: &Publisher) -> Value {
    snapshot(publisher)["collections"]["rows"].clone()
}

pub(super) fn snapshot(publisher: &Publisher) -> Value {
    let frame = publisher.frame_for(None, "s", false);
    let crate::generated::core_v1::FrameBody::Snapshot(snapshot) = frame.body else {
        panic!("not a snapshot")
    };
    serde_json::to_value(snapshot.snapshot).unwrap()
}

/// 行的内容（去掉版本：活会话里每行在不同的批里变，冷读一批建出）。
fn contents(rows: &Value) -> Vec<Value> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let mut row = row.clone();
            row.as_object_mut().unwrap().remove("v");
            row
        })
        .collect()
}

/// K-I2：同一段对话，冷读（视图）与活会话（事件）是同一组行，id、字段与次序都相同。
#[test]
fn a_cold_read_and_the_live_session_give_the_same_rows() {
    let (messages, events) = conversation();
    let (_, publisher) = live(events);
    let cold_view = SessionView {
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
    let live_rows = contents(&rows_of(&publisher));
    assert_eq!(live_rows, contents(&rows_of(&cold)));
    let ids: Vec<&str> = live_rows
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            TURN,
            "msg_u",
            "msg_a:reasoning:0",
            "msg_a",
            "toolu_1",
            "msg_b"
        ],
        "rows in display order (createdAtSeq): turn header, input, thinking, text, tool, last text"
    );
    let tool = live_rows.iter().find(|row| row["id"] == "toolu_1").unwrap();
    assert_eq!(tool["status"], "completed");
    assert_eq!(tool["output"], json!({"text": "ok"}));
    assert_eq!(tool["inputText"], "{\"a\":1}");
}

/// K-I3：流式增量对到它最后落成的行（第一段 put、之后 append）；最终的消息里没有的流式行去掉（同一批）。
#[test]
fn deltas_land_on_their_final_rows_and_strays_go() {
    let started = KernelEvent::TurnStarted {
        turn: TurnId::new(TURN),
        command: None,
        origin: TurnOrigin::Input,
        started_at: 1,
        works: Vec::new(),
    };
    let streamed = vec![
        started,
        delta("msg_a", 0, None, BlockDelta::Thinking("half".to_owned())),
        delta("msg_a", 1, None, BlockDelta::Text("He".to_owned())),
        delta("msg_a", 1, None, BlockDelta::Text("y".to_owned())),
    ];
    let (_, publisher) = live(streamed.clone());
    let streaming = publisher.entity(rows::ROWS, "msg_a").unwrap();
    assert_eq!(streaming["text"], "Hey");
    assert_eq!(streaming["state"], "streaming");
    assert_eq!(streaming["stable"], false);
    assert!(publisher.entity(rows::ROWS, "msg_a:reasoning:0").is_some());
    // 中止时没写完的思考块不进最终的消息（query 的 finish_aborted）：它的流式行随 MessageDone 去掉
    let mut done = streamed;
    done.push(KernelEvent::MessageDone {
        message: assistant(
            "msg_a",
            vec![ContentBlock::Text {
                text: "Hey".to_owned(),
            }],
        ),
    });
    let (_, publisher) = live(done);
    assert!(publisher.entity(rows::ROWS, "msg_a:reasoning:0").is_none());
    let text = publisher.entity(rows::ROWS, "msg_a").unwrap();
    assert_eq!(
        (text["state"].clone(), text["stable"].clone()),
        (json!("done"), json!(true))
    );
    assert_eq!(
        snapshot(&publisher)["state"]["rowsWindow"],
        json!({"totalCount": 2, "firstId": TURN, "earlier": false})
    );
}

/// 回合结束：没等到 MessageDone 的流式行去掉；还没结束的工具 cancelled（C7）；回合头 cancelled、phase completed。
#[test]
fn a_stopped_turn_drops_unfinished_streams_and_cancels_open_tools() {
    let (_, publisher) = live(vec![
        KernelEvent::TurnStarted {
            turn: TurnId::new(TURN),
            command: None,
            origin: TurnOrigin::Input,
            started_at: 1,
            works: Vec::new(),
        },
        delta(
            "msg_a",
            0,
            Some("toolu_1"),
            BlockDelta::InputJson("{".to_owned()),
        ),
        delta("msg_x", 0, None, BlockDelta::Text("lost".to_owned())),
        KernelEvent::TurnFinished {
            turn: TurnId::new(TURN),
            outcome: TurnOutcome::Interrupted,
            usage: None,
            time: super::kit::ENDED,
            files: Vec::new(),
        },
    ]);
    assert!(publisher.entity(rows::ROWS, "msg_x").is_none());
    assert_eq!(
        publisher.entity(rows::ROWS, "toolu_1").unwrap()["status"],
        "cancelled"
    );
    assert_eq!(
        publisher.entity(rows::ROWS, TURN).unwrap()["state"],
        "cancelled"
    );
    let state = snapshot(&publisher)["state"].clone();
    assert_eq!(state["control"]["phase"], "completed");
    assert_eq!(state["control"]["lastOutcome"], "interrupted");
    assert_eq!(state["control"]["turnId"], Value::Null);
}

/// 窗口（02 §2.3）：快照只带尾部 60 行，`rowsWindow` 给总数与最早一行；createdAtSeq 一行一个。没有回合记录的输入各开一个回合（§75.3）：每条输入一行回合头、一行输入。
#[test]
fn the_snapshot_carries_the_last_rows_and_the_window() {
    let messages: Vec<Message> = (0..SESSION_SNAPSHOT_ROWS + 5)
        .map(|n| user(&format!("msg_{n:03}"), "x"))
        .collect();
    let (_, content) = Projection::build(&view(messages, Vec::new()), origin(false));
    let mut publisher = Publisher::new("t", "e").with_window(
        rows::ROWS,
        rows::CREATED_AT_SEQ,
        SESSION_SNAPSHOT_ROWS,
        usize::MAX,
    );
    publisher.adopt(content);
    let snapshot = snapshot(&publisher);
    let rows = snapshot["collections"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), SESSION_SNAPSHOT_ROWS);
    assert_eq!(
        snapshot["state"]["rowsWindow"],
        json!({"totalCount": 2 * (SESSION_SNAPSHOT_ROWS + 5), "firstId": "turn_of_msg_000", "earlier": false})
    );
    let mut seqs: Vec<u64> = rows
        .iter()
        .map(|row| row["createdAtSeq"].as_u64().unwrap())
        .collect();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(seqs.len(), SESSION_SNAPSHOT_ROWS);
    // 130 行里的最后 60 行，从第 70 行（一行回合头）起
    assert_eq!(seqs[0], u64::try_from(SESSION_SNAPSHOT_ROWS + 10).unwrap());
    assert_eq!(rows[0]["kind"], "turnHeader");
}

fn queued(id: &str) -> QueuedInput {
    QueuedInput::new(CommandId::new(id), id.to_owned(), Delivery::Queue)
}

/// 队列整份来：新的 put、没了的 remove；扣住且非空时 admission 是 choice（02 §4.1）。
#[test]
fn the_queue_follows_the_whole_list_and_a_hold_asks_for_a_choice() {
    let (_, publisher) = live(vec![
        KernelEvent::QueueChanged {
            items: vec![queued("c2"), queued("c3")],
        },
        KernelEvent::QueueChanged {
            items: vec![queued("c3")],
        },
        KernelEvent::QueueHeld {
            hold: Some(QueueHold::Stopped),
        },
    ]);
    assert!(publisher.entity(QUEUE, "c2").is_none());
    let c3 = publisher.entity(QUEUE, "c3").unwrap();
    assert_eq!(c3["order"], json!({"admissionSeq": 2}));
    let state = snapshot(&publisher)["state"].clone();
    assert_eq!(
        state["queuePolicy"],
        json!({"autoDrain": false, "pauseReason": "stopped"})
    );
    assert_eq!(state["admission"]["input"]["mode"], "choice");
    assert_eq!(
        state["admission"]["input"]["allowed"],
        json!(["keepQueue", "discardQueue"])
    );
}

/// 输入的结局（02 §1.5）：内核的 InputFate 换算；终态只留最近 INPUT_FATE_RETAIN 条。
#[test]
fn input_fates_map_to_the_closed_set_and_old_ends_are_dropped() {
    let fate = |id: &str, fate: InputFate| KernelEvent::InputFate {
        input: CommandId::new(id),
        fate,
        at: 7,
    };
    let mut events = vec![
        fate("q", InputFate::Queued),
        fate(
            "s",
            InputFate::Started {
                turn: TurnId::new(TURN),
                joined: Joined::Opened,
            },
        ),
        fate(
            "b",
            InputFate::Dropped {
                why: DropReason::Blocked,
            },
        ),
    ];
    events.extend((0..INPUT_FATE_RETAIN).map(|n| {
        fate(
            &format!("e{n}"),
            InputFate::Ended {
                turn: TurnId::new(TURN),
                end: InputEnd::Cancelled,
            },
        )
    }));
    let (_, publisher) = live(events);
    assert_eq!(publisher.entity(INPUTS, "q").unwrap()["fate"], "queued");
    assert_eq!(publisher.entity(INPUTS, "s").unwrap()["turnId"], TURN);
    // 被拦下的是最早的终态：保留上限挤掉了它
    assert!(publisher.entity(INPUTS, "b").is_none());
    let ended = publisher.entity(INPUTS, "e0").unwrap();
    assert_eq!(ended["fate"], "cancelled");
    assert_eq!(ended["at"], 7);
}

/// K-X1：交互的实体 id 是 tool_use；权限询问期间工具行 pendingApproval；回答之后回到 running。C6：没有建议规则时没有 allowAlways。
#[test]
fn an_interaction_is_named_by_its_tool_call() {
    let ask = InteractionRequest::Permission {
        tool_use: ToolUseId::new("toolu_1"),
        tool: "Write".to_owned(),
        input: json!({}),
        message: "Write a.txt?".to_owned(),
        suggestions: Vec::new(),
        rule_seed: None,
        asked_by: None,
        machine: Default::default(),
        once: false,
    };
    let (projection, publisher) = live(vec![
        KernelEvent::TurnStarted {
            turn: TurnId::new(TURN),
            command: None,
            origin: TurnOrigin::Input,
            started_at: 1,
            works: Vec::new(),
        },
        KernelEvent::ToolStarted {
            tool_use: ToolUseId::new("toolu_1"),
            name: "Write".to_owned(),
            input: json!({}),
            started_at: 2,
        },
        KernelEvent::InteractionAsked {
            interaction: InteractionId::new("interaction_random"),
            request: ask,
        },
    ]);
    let entity = publisher.entity(INTERACTIONS, "toolu_1").unwrap();
    assert_eq!(entity["kind"], "permission");
    assert_eq!(entity["payload"]["options"], json!(["allowOnce", "deny"]));
    assert_eq!(
        publisher.entity(rows::ROWS, "toolu_1").unwrap()["status"],
        "pendingApproval"
    );
    assert_eq!(
        projection.interaction("toolu_1").map(|(id, _)| id.as_str()),
        Some("interaction_random")
    );
}

pub(super) fn attached() -> rust_cli_kernel_api::input_extras::InputRecord {
    rust_cli_kernel_api::input_extras::InputRecord {
        files: vec![rust_cli_kernel_api::input_extras::AttachedFile {
            path: "/Users/dev/a/notes.md".to_owned(),
            name: "notes.md".to_owned(),
            media_type: "text/markdown".to_owned(),
            size: 12,
        }],
        ..rust_cli_kernel_api::input_extras::InputRecord::new(MessageId::new("msg_u"))
    }
}

/// 附件（protocol-v5.md §15.3.3）：活会话（`InputRecorded` 在 `MessageDone` 之前）与冷读（视图的 `inputs`）给出同一行，
/// 附件是 v4 `AttachmentRef` 的形状。
#[test]
fn the_input_row_carries_its_attachments_live_and_cold() {
    let (messages, mut events) = conversation();
    events.insert(1, KernelEvent::InputRecorded { record: attached() });
    let (_, publisher) = live(events);
    let cold_view = SessionView {
        inputs: vec![attached()],
        ..view(messages, vec![record()])
    };
    let (_, content) = Projection::build(&cold_view, origin(false));
    let mut cold = Publisher::new("t", "e");
    cold.adopt(content);
    let expected = json!([{
        "ref": "/Users/dev/a/notes.md",
        "fileName": "notes.md",
        "mime": "text/markdown",
        "bytes": 12,
    }]);
    for rows in [rows_of(&publisher), rows_of(&cold)] {
        let input = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "msg_u")
            .unwrap()
            .clone();
        assert_eq!(input["attachments"], expected);
    }
}

/// 停止时在等回答的工具（02 §11 C7）：活会话按 `ToolDone{cancelled}`，冷读按回合记录的结局，两边都是 cancelled；冷读的回合头
/// 是回合真实的结局（以前一律 completed，§15.12 第 2、12 项）。
#[test]
fn a_stopped_tool_and_turn_read_as_cancelled_live_and_cold() {
    let (messages, mut events) = conversation();
    for event in &mut events {
        if let KernelEvent::ToolDone { cancelled, .. } = event {
            *cancelled = true;
        }
        if let KernelEvent::TurnFinished { outcome, .. } = event {
            *outcome = TurnOutcome::Interrupted;
        }
    }
    let (_, publisher) = live(events);
    let ended = TurnRecord {
        ended: super::kit::ended(InputEnd::Cancelled, vec![ToolUseId::new("toolu_1")]),
        ..record()
    };
    let (_, content) = Projection::build(&view(messages, vec![ended]), origin(false));
    let mut cold = Publisher::new("t", "e");
    cold.adopt(content);
    for rows in [rows_of(&publisher), rows_of(&cold)] {
        let row = |id: &str| {
            rows.as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == id)
                .unwrap()
                .clone()
        };
        assert_eq!(row("toolu_1")["status"], "cancelled");
        assert_eq!(row(TURN)["state"], "cancelled");
    }
}

const COMPACTION: &str = "turn_c";

fn boundary(trigger: rust_cli_kernel_api::message::CompactTrigger) -> Message {
    Message::System(
        rust_cli_kernel_api::message::SystemMessage::CompactBoundary(
            rust_cli_kernel_api::message::CompactBoundary {
                id: MessageId::new("msg_bd"),
                trigger,
                pre_tokens: 7,
                summary: MessageId::new("msg_sum"),
                preserved: None,
                discovered_tools: Vec::new(),
            },
        ),
    )
}

fn summary() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_sum"),
        content: vec![ContentBlock::Text {
            text: "summary".to_owned(),
        }],
        origin: UserOrigin::CompactSummary,
    })
}

fn compaction_started() -> KernelEvent {
    KernelEvent::TurnStarted {
        turn: TurnId::new(COMPACTION),
        command: Some(CommandId::new("k1")),
        origin: TurnOrigin::Compaction,
        started_at: 1,
        works: Vec::new(),
    }
}

/// 压缩（protocol-v5.md §17.3）：成功只多一条 timelineMarker 行，`detail` 是 v4 的 compact marker（UI 按它读）；冷读与活会话同一行；
/// 压缩中 `admission.compact` 拒绝（compactLocked），结束后放开。
#[test]
fn a_compaction_adds_one_marker_row_with_the_v4_detail() {
    let trigger = rust_cli_kernel_api::message::CompactTrigger::Manual;
    let (_, running) = live(vec![compaction_started()]);
    let state = snapshot(&running)["state"].clone();
    assert_eq!(state["control"]["activeWorks"][0]["kind"], "compact");
    assert_eq!(
        state["admission"]["compact"],
        json!({"allow": false, "reason": "admission.compactLocked"})
    );
    let (_, publisher) = live(vec![
        compaction_started(),
        KernelEvent::MessageDone {
            message: boundary(trigger),
        },
        KernelEvent::MessageDone { message: summary() },
        KernelEvent::CompactDone {
            boundary: MessageId::new("msg_bd"),
        },
        KernelEvent::TurnFinished {
            turn: TurnId::new(COMPACTION),
            outcome: TurnOutcome::Success,
            usage: None,
            time: super::kit::ENDED,
            files: Vec::new(),
        },
    ]);
    let rows = contents(&rows_of(&publisher));
    let ids: Vec<&str> = rows.iter().map(|row| row["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [COMPACTION, "msg_bd"]);
    assert_eq!(rows[1]["kind"], "timelineMarker");
    assert_eq!(rows[1]["marker"], "compact");
    assert_eq!(
        rows[1]["detail"],
        json!({"type": "compact", "origin": "manual", "status": "success", "tokensBefore": 7})
    );
    assert_eq!(
        snapshot(&publisher)["state"]["admission"]["compact"],
        json!({"allow": true})
    );
    let record = TurnRecord {
        turn: TurnId::new(COMPACTION),
        first: MessageId::new("msg_bd"),
        origin: TurnOrigin::Compaction,
        started_at: 1,
        command: Some(CommandId::new("k1")),
        ended: super::kit::ended(InputEnd::Completed, Vec::new()),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let (_, content) = Projection::build(
        &view(vec![boundary(trigger), summary()], vec![record]),
        origin(false),
    );
    let mut cold = Publisher::new("t", "e").with_window(
        rows::ROWS,
        rows::CREATED_AT_SEQ,
        SESSION_SNAPSHOT_ROWS,
        usize::MAX,
    );
    cold.adopt(content);
    assert_eq!(rows, contents(&rows_of(&cold)));
    // 自动与反应式的压缩在 v4 的 marker 里都是 auto
    let (_, auto) = live(vec![
        compaction_started(),
        KernelEvent::MessageDone {
            message: boundary(rust_cli_kernel_api::message::CompactTrigger::Reactive),
        },
    ]);
    assert_eq!(
        auto.entity(rows::ROWS, "msg_bd").unwrap()["detail"]["origin"],
        "auto"
    );
}

#[path = "projection_config_tests.rs"]
mod config;

#[path = "projection_source_tests.rs"]
mod source;

#[path = "projection_inputs_tests.rs"]
mod inputs;
