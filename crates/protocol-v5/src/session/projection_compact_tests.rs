//! 压缩标记的一生（protocol-v5.md §47.1、§50）：手动压缩的回合开始时一行 `running`，边界落库时换成成功的标记，失败时同一行改成 `failed`，
//! 停止时改成 `cancelled`；活会话与由视图建同一行。

use rust_cli_kernel_api::compaction::{LoopCompaction, LoopCompactionEnd};
use rust_cli_kernel_api::ids::{CommandId, MessageId, TurnId};
use rust_cli_kernel_api::message::{
    CompactBoundary, CompactTrigger, ContentBlock, Message, SystemMessage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::session_api::{
    Delivery, InputEnd, KernelEvent, QueuedInput, Running, SessionView, TurnOrigin, TurnOutcome,
    TurnStart,
};
use rust_cli_kernel_api::store::TurnRecord;
use rust_cli_kernel_api::user_command::LOCAL_COMMAND_OUTPUT_SOURCE;
use serde_json::{Value, json};

use super::super::kit::{ENDED, ended};
use super::super::tests::{live, origin, snapshot, view};
use super::*;

const TURN: &str = "turn_c";
/// 压缩的回合在跑与失败时的标记行（`<回合 id>:compact`）。
const MARKER: &str = "turn_c:compact";
const STARTED_AT: u64 = 1_700;
const COMMAND: &str = "k1";

fn start() -> TurnStart {
    TurnStart {
        turn: TurnId::new(TURN),
        command: Some(CommandId::new(COMMAND)),
        origin: TurnOrigin::Compaction,
        started_at: STARTED_AT,
        works: Vec::new(),
    }
}

fn started() -> KernelEvent {
    let start = start();
    KernelEvent::TurnStarted {
        turn: start.turn,
        command: start.command,
        origin: start.origin,
        started_at: start.started_at,
        works: Vec::new(),
    }
}

fn boundary() -> Message {
    Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new("msg_bd"),
        trigger: CompactTrigger::Manual,
        pre_tokens: 7,
        summary: MessageId::new("msg_sum"),
        preserved: None,
        discovered_tools: Vec::new(),
    }))
}

/// 压缩失败时提交的只有本地命令的记录（meta，不成行）。
fn stderr_record() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_record"),
        content: vec![ContentBlock::Text {
            text: "stderr".to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: LOCAL_COMMAND_OUTPUT_SOURCE.to_owned(),
        },
    })
}

fn finished(outcome: TurnOutcome) -> KernelEvent {
    KernelEvent::TurnFinished {
        turn: TurnId::new(TURN),
        outcome,
        usage: None,
        time: ENDED,
        files: Vec::new(),
    }
}

fn failed() -> TurnOutcome {
    TurnOutcome::Failed {
        message: "Not enough messages to compact.".to_owned(),
        cause: None,
    }
}

fn record(first: &str, outcome: Option<InputEnd>) -> TurnRecord {
    TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new(first),
        origin: TurnOrigin::Compaction,
        started_at: STARTED_AT,
        command: Some(CommandId::new(COMMAND)),
        ended: outcome.and_then(|outcome| ended(outcome, Vec::new())),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    }
}

fn built(view: &SessionView) -> Publisher {
    let (_, content) = Projection::build(view, origin(false));
    let mut publisher = Publisher::new("ws/a/session/ses_1", "e");
    publisher.adopt(content);
    publisher
}

fn row(publisher: &Publisher, id: &str) -> Option<Value> {
    publisher.entity(rows::ROWS, id).map(|fields| json!(fields))
}

/// 修 bug（19 §47 R-COMPACT-STARTED）：以前压缩的回合在跑时时间线上没有压缩标记（只在结束时才有），界面看不到「压缩中」，按开始的标记排队、
/// 停止的用例走不下去。现在回合开始时就有一行 `running`；边界落库时它让给成功的标记（同一个命令，界面据此把两者对上）。
#[test]
fn a_running_compaction_shows_a_running_marker_until_its_boundary() {
    let (_, running) = live(vec![started()]);
    let marker = row(&running, MARKER).expect("a running compaction has a marker");
    assert_eq!(marker["kind"], "timelineMarker");
    assert_eq!(marker["marker"], "compact");
    assert_eq!(
        marker["detail"],
        json!({"type": "compact", "origin": "manual", "status": "running"})
    );
    assert_eq!(marker["stable"], false, "a running marker still changes");
    assert_eq!(marker["turnId"], TURN);
    assert_eq!(marker["createdAt"], rows::millis(STARTED_AT));
    assert_eq!(marker["sourceCommandId"], COMMAND);
    let (_, done) = live(vec![
        started(),
        KernelEvent::MessageDone {
            message: boundary(),
        },
    ]);
    assert!(
        row(&done, MARKER).is_none(),
        "the boundary takes the place of the running marker"
    );
    let success = row(&done, "msg_bd").unwrap();
    assert_eq!(success["detail"]["status"], "success");
    assert_eq!(success["sourceCommandId"], COMMAND);
}

/// 回合中途由视图建的投影（订阅、激活）：压缩的回合在跑、边界还没落库时有同一行开始的标记；边界已经在视图里时只有成功的标记。
#[test]
fn a_projection_built_mid_compaction_has_the_same_marker() {
    let (_, live_running) = live(vec![started()]);
    let before = SessionView {
        running: Some(Running::Turn(start())),
        ..view(Vec::new(), Vec::new())
    };
    let marker = row(&built(&before), MARKER);
    assert!(
        marker.is_some(),
        "a projection built mid-compaction has the marker"
    );
    assert_eq!(
        marker,
        row(&live_running, MARKER),
        "the same running marker"
    );
    let after = SessionView {
        running: Some(Running::Turn(start())),
        ..view(vec![boundary()], vec![record("msg_bd", None)])
    };
    let after = built(&after);
    assert!(row(&after, MARKER).is_none());
    assert_eq!(
        row(&after, "msg_bd").unwrap()["detail"]["status"],
        "success"
    );
}

/// 压缩失败：开始的那一行原地改成失败（同一个行号、带同一个命令），活会话与冷读（回合记录的结局）同一行。
#[test]
fn a_failed_compaction_turns_the_running_marker_into_the_failed_one() {
    let (_, running) = live(vec![started()]);
    let (_, failed_live) = live(vec![started(), finished(failed())]);
    let before = row(&running, MARKER).unwrap();
    let after = row(&failed_live, MARKER).unwrap();
    assert_eq!(
        after["detail"],
        json!({"type": "compact", "origin": "manual", "status": "failed"})
    );
    assert_eq!(after["stable"], true);
    assert_eq!(after["sourceCommandId"], COMMAND);
    assert_eq!(after[rows::CREATED_AT_SEQ], before[rows::CREATED_AT_SEQ]);
    let cold = built(&view(
        vec![stderr_record()],
        vec![record("msg_record", Some(InputEnd::Failed))],
    ));
    assert_eq!(row(&cold, MARKER), Some(after));
}

/// 修 bug（protocol-v5.md §50.1）：停止的压缩以前去掉开始的标记（契约里没有 `cancelled`），v4 显示「已取消」。现在开始的那一行原地改成
/// `cancelled`（同一个行号、带同一个命令），活会话与冷读（回合记录的结局是被中断）同一行。
#[test]
fn a_stopped_compaction_turns_the_running_marker_into_the_cancelled_one() {
    let (_, running) = live(vec![started()]);
    let (_, stopped) = live(vec![started(), finished(TurnOutcome::Interrupted)]);
    let before = row(&running, MARKER).unwrap();
    let after = row(&stopped, MARKER).expect("a stopped compaction keeps its marker");
    assert_eq!(
        after["detail"],
        json!({"type": "compact", "origin": "manual", "status": "cancelled"})
    );
    assert_eq!(after["stable"], true);
    assert_eq!(after["sourceCommandId"], COMMAND);
    assert_eq!(after[rows::CREATED_AT_SEQ], before[rows::CREATED_AT_SEQ]);
    let cold = built(&view(
        vec![stderr_record()],
        vec![record("msg_record", Some(InputEnd::Cancelled))],
    ));
    assert_eq!(row(&cold, MARKER), Some(after));
}

/// 排队的压缩（§17.3；从 projection_tests.rs 挪来，800 行上限）：`kind: compact`、正文为空；有它时 `admission.compact` 拒绝；没有压缩、只有别的回合在跑时放开（内核会排队）；
/// 草稿不压缩。
#[test]
fn a_queued_compaction_is_a_compact_item_and_locks_the_gate() {
    let item = |id: &str, compact: bool| QueuedInput {
        compact,
        ..QueuedInput::new(
            CommandId::new(id),
            if compact {
                String::new()
            } else {
                id.to_owned()
            },
            Delivery::Queue,
        )
    };
    let started = KernelEvent::TurnStarted {
        turn: TurnId::new("turn_1"),
        command: None,
        origin: TurnOrigin::Input,
        started_at: 1,
        works: Vec::new(),
    };
    let (_, busy) = live(vec![started.clone()]);
    assert_eq!(
        snapshot(&busy)["state"]["admission"]["compact"],
        json!({"allow": true})
    );
    let (_, publisher) = live(vec![
        started,
        KernelEvent::QueueChanged {
            items: vec![item("c2", false), item("k1", true)],
        },
    ]);
    let item = publisher.entity(QUEUE, "k1").unwrap();
    assert_eq!(
        (item["kind"].clone(), item["text"].clone()),
        (json!("compact"), json!(""))
    );
    assert_eq!(publisher.entity(QUEUE, "c2").unwrap()["kind"], "input");
    assert_eq!(
        snapshot(&publisher)["state"]["admission"]["compact"],
        json!({"allow": false, "reason": "admission.compactLocked"})
    );
    let draft = Origin {
        draft: true,
        ..origin(true)
    };
    let (_, content) = Projection::build(&view(Vec::new(), Vec::new()), draft);
    assert_eq!(
        content.state["admission"]["compact"],
        json!({"allow": false, "reason": "admission.draft"})
    );
}

// ── 主循环里的压缩（protocol-v5.md §50.2）：回合记录里的每一项一行 `<回合 id>:compact:<序号>`，活会话与由视图建同一行 ──

const LOOP_TURN: &str = "turn_l";
/// 主循环里第一次压缩的标记行。
const LOOP_MARKER: &str = "turn_l:compact:0";
const LOOP_STARTED_AT: u64 = 1_800;

fn loop_start() -> TurnStart {
    TurnStart {
        turn: TurnId::new(LOOP_TURN),
        command: Some(CommandId::new("c1")),
        origin: TurnOrigin::Input,
        started_at: 1_000,
        works: Vec::new(),
    }
}

fn loop_started() -> KernelEvent {
    let start = loop_start();
    KernelEvent::TurnStarted {
        turn: start.turn,
        command: start.command,
        origin: start.origin,
        started_at: start.started_at,
        works: Vec::new(),
    }
}

fn prompt() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_u"),
        content: vec![ContentBlock::Text {
            text: "hi".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn answer() -> Message {
    Message::Assistant(rust_cli_kernel_api::message::AssistantMessage {
        id: MessageId::new("msg_a"),
        content: vec![ContentBlock::Text {
            text: "ok".to_owned(),
        }],
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn auto_boundary() -> Message {
    Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new("msg_bd_auto"),
        trigger: CompactTrigger::Auto,
        pre_tokens: 9,
        summary: MessageId::new("msg_sum_auto"),
        preserved: None,
        discovered_tools: Vec::new(),
    }))
}

fn done(message: Message) -> KernelEvent {
    KernelEvent::MessageDone { message }
}

fn loop_compaction(ended: Option<LoopCompactionEnd>) -> LoopCompaction {
    LoopCompaction {
        trigger: CompactTrigger::Auto,
        started_at: LOOP_STARTED_AT,
        after: MessageId::new("msg_u"),
        ended,
    }
}

fn loop_recorded(ended: Option<LoopCompactionEnd>) -> KernelEvent {
    KernelEvent::CompactionRecorded {
        turn: TurnId::new(LOOP_TURN),
        index: 0,
        compaction: loop_compaction(ended),
    }
}

fn loop_record(end: Option<LoopCompactionEnd>, outcome: Option<InputEnd>) -> TurnRecord {
    TurnRecord {
        turn: TurnId::new(LOOP_TURN),
        first: MessageId::new("msg_u"),
        origin: TurnOrigin::Input,
        started_at: 1_000,
        command: Some(CommandId::new("c1")),
        ended: outcome.and_then(|outcome| ended(outcome, Vec::new())),
        fell_back: None,
        works: Vec::new(),
        compactions: vec![loop_compaction(end)],
    }
}

fn seq(publisher: &Publisher, id: &str) -> Value {
    row(publisher, id).unwrap()[rows::CREATED_AT_SEQ].clone()
}

/// 修 bug（protocol-v5.md §50.2）：以前主循环里的自动压缩在跑时没有标记（视图里没有这个事实），v4 有「压缩中」。现在用户行之后一行
/// `running`（来源 auto、开始的时刻、不稳定、没有命令）；边界提交时边界行接过它的行号，之后的 `Succeeded` 不再出行。
#[test]
fn a_main_loop_compaction_shows_a_running_marker_until_its_boundary() {
    let (_, running) = live(vec![loop_started(), done(prompt()), loop_recorded(None)]);
    let marker = row(&running, LOOP_MARKER).expect("a running main-loop compaction has a marker");
    assert_eq!(marker["kind"], "timelineMarker");
    assert_eq!(
        marker["detail"],
        json!({"type": "compact", "origin": "auto", "status": "running"})
    );
    assert_eq!(marker["stable"], false);
    assert_eq!(marker["turnId"], LOOP_TURN);
    assert_eq!(marker["createdAt"], rows::millis(LOOP_STARTED_AT));
    assert!(marker.get("sourceCommandId").is_none(), "{marker}");
    let after_prompt = seq(&running, "msg_u").as_u64().unwrap();
    assert!(marker[rows::CREATED_AT_SEQ].as_u64().unwrap() > after_prompt);
    let (_, compacted) = live(vec![
        loop_started(),
        done(prompt()),
        loop_recorded(None),
        done(auto_boundary()),
        loop_recorded(Some(LoopCompactionEnd::Succeeded)),
    ]);
    assert!(row(&compacted, LOOP_MARKER).is_none());
    let success = row(&compacted, "msg_bd_auto").unwrap();
    assert_eq!(
        success["detail"],
        json!({"type": "compact", "origin": "auto", "status": "success", "tokensBefore": 9})
    );
    assert_eq!(
        success[rows::CREATED_AT_SEQ],
        marker[rows::CREATED_AT_SEQ],
        "the boundary takes the row number of the running marker"
    );
}

/// 回合中途由视图建的投影：在跑回合的记录里没有结局的那一项出同一行 `running`，排在 `after` 那条消息之后。
#[test]
fn a_projection_built_mid_main_loop_compaction_has_the_same_marker() {
    let (_, running) = live(vec![loop_started(), done(prompt()), loop_recorded(None)]);
    let view = SessionView {
        running: Some(Running::Turn(loop_start())),
        ..view(vec![prompt()], vec![loop_record(None, None)])
    };
    let built = built(&view);
    assert!(
        row(&built, LOOP_MARKER).is_some(),
        "a projection built mid-compaction has the marker"
    );
    assert_eq!(row(&built, LOOP_MARKER), row(&running, LOOP_MARKER));
    assert_eq!(seq(&built, "msg_u"), seq(&running, "msg_u"));
}

/// 开始之后失败：同一行改成 `failed`（同一个行号），回合照常继续；冷读按回合记录出同一行、同一个位置（在 `after` 之后、后面的回答之前）。
#[test]
fn a_failed_main_loop_compaction_keeps_its_row_live_and_cold() {
    let (_, failed) = live(vec![
        loop_started(),
        done(prompt()),
        loop_recorded(None),
        loop_recorded(Some(LoopCompactionEnd::Failed)),
        done(answer()),
        KernelEvent::TurnFinished {
            turn: TurnId::new(LOOP_TURN),
            outcome: TurnOutcome::Success,
            usage: None,
            time: ENDED,
            files: Vec::new(),
        },
    ]);
    let marker = row(&failed, LOOP_MARKER).unwrap();
    assert_eq!(
        marker["detail"],
        json!({"type": "compact", "origin": "auto", "status": "failed"})
    );
    assert_eq!(marker["stable"], true);
    let cold = built(&view(
        vec![prompt(), answer()],
        vec![loop_record(
            Some(LoopCompactionEnd::Failed),
            Some(InputEnd::Completed),
        )],
    ));
    assert_eq!(row(&cold, LOOP_MARKER), Some(marker));
    assert_eq!(seq(&cold, "msg_a"), seq(&failed, "msg_a"));
}

/// 压缩中停止：回合结束之前那一项写成 `Cancelled`，同一行改成 `cancelled`；冷读同一行。
#[test]
fn a_stopped_main_loop_compaction_is_cancelled_live_and_cold() {
    let (_, stopped) = live(vec![
        loop_started(),
        done(prompt()),
        loop_recorded(None),
        loop_recorded(Some(LoopCompactionEnd::Cancelled)),
        KernelEvent::TurnFinished {
            turn: TurnId::new(LOOP_TURN),
            outcome: TurnOutcome::Interrupted,
            usage: None,
            time: ENDED,
            files: Vec::new(),
        },
    ]);
    let marker = row(&stopped, LOOP_MARKER).unwrap();
    assert_eq!(
        marker["detail"],
        json!({"type": "compact", "origin": "auto", "status": "cancelled"})
    );
    let cold = built(&view(
        vec![prompt()],
        vec![loop_record(
            Some(LoopCompactionEnd::Cancelled),
            Some(InputEnd::Cancelled),
        )],
    ));
    assert_eq!(row(&cold, LOOP_MARKER), Some(marker));
}

/// 进程在主循环压缩的中途退出：那一项没有结局、回合也不在跑，冷读不出这一行（§50.7 第 2 项）；成功的那一项只有边界行。
#[test]
fn an_unended_or_successful_main_loop_compaction_of_a_past_turn_has_no_marker() {
    for ended in [None, Some(LoopCompactionEnd::Succeeded)] {
        let cold = built(&view(vec![prompt()], vec![loop_record(ended, None)]));
        assert!(row(&cold, LOOP_MARKER).is_none(), "{ended:?}");
    }
}

// ── 冷读的保留段（R-COMPACT-TAIL-COLD，protocol-v5.md §62.1）：由视图建投影时按存储的先后走 ──

const THREE: &str = "turn_3";

fn user(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn reply(id: &str, text: &str) -> Message {
    Message::Assistant(rust_cli_kernel_api::message::AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        stop_reason: None,
        usage: None,
    })
}

/// 带保留段的手动压缩的边界：保留 `head..=tail`，摘要是 `summary`。
fn kept(id: &str, summary: &str, (head, tail): (&str, &str)) -> Message {
    Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new(id),
        trigger: CompactTrigger::Manual,
        pre_tokens: 7,
        summary: MessageId::new(summary),
        preserved: Some(rust_cli_kernel_api::message::PreservedSegment {
            head: MessageId::new(head),
            anchor: MessageId::new(summary),
            tail: MessageId::new(tail),
        }),
        discovered_tools: Vec::new(),
    }))
}

fn compact_summary(id: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "summary".to_owned(),
        }],
        origin: UserOrigin::CompactSummary,
    })
}

fn ids(messages: &[&Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| message.id().as_str().to_owned())
        .collect()
}

/// 冷读的形状（保留段接在摘要之后）挪回边界之前；活会话的形状（本来就在边界之前）不动；两个压缩点时从后往前还原：
/// 后一次压缩把上一次的保留段整段再保留时，冷读里它只在最后一个摘要之后。
#[test]
fn stored_order_moves_a_cold_preserved_segment_back_before_its_boundary() {
    let stored = [
        user("msg_u3", "seed three"),
        reply("msg_a3", "three ok"),
        kept("msg_bd", "msg_sum", ("msg_a3", "msg_a3")),
        compact_summary("msg_sum"),
    ];
    let order = ["msg_u3", "msg_a3", "msg_bd", "msg_sum"];
    assert_eq!(
        ids(&super::stored_order(&stored)),
        order,
        "the live shape stays"
    );
    let cold = [
        stored[0].clone(),
        stored[2].clone(),
        stored[3].clone(),
        stored[1].clone(),
    ];
    assert_eq!(ids(&super::stored_order(&cold)), order);
    // 第二次压缩从第一次的保留段起保留（`msg_a1..=msg_a2`）：冷读是 earlier（`msg_u1`、第一个边界与摘要）＋ 第二个边界、摘要、保留段
    let twice = [
        user("msg_u1", "one"),
        kept("msg_bd1", "msg_sum1", ("msg_a1", "msg_a1")),
        compact_summary("msg_sum1"),
        kept("msg_bd2", "msg_sum2", ("msg_a1", "msg_a2")),
        compact_summary("msg_sum2"),
        reply("msg_a1", "one ok"),
        user("msg_u2", "two"),
        reply("msg_a2", "two ok"),
    ];
    assert_eq!(
        ids(&super::stored_order(&twice)),
        [
            "msg_u1", "msg_a1", "msg_bd1", "msg_sum1", "msg_u2", "msg_a2", "msg_bd2", "msg_sum2"
        ]
    );
    // 冷读之后会话里又压缩：上一个边界的保留段仍是冷读的形状，最后一个是存储的先后
    let reopened = [
        user("msg_u1", "one"),
        kept("msg_bd1", "msg_sum1", ("msg_a1", "msg_a1")),
        compact_summary("msg_sum1"),
        reply("msg_a1", "one ok"),
        user("msg_u2", "two"),
        reply("msg_a2", "two ok"),
        kept("msg_bd2", "msg_sum2", ("msg_a2", "msg_a2")),
        compact_summary("msg_sum2"),
    ];
    assert_eq!(
        ids(&super::stored_order(&reopened)),
        [
            "msg_u1", "msg_a1", "msg_bd1", "msg_sum1", "msg_u2", "msg_a2", "msg_bd2", "msg_sum2"
        ]
    );
}

/// 修 bug（R-COMPACT-TAIL-COLD）：同一段对话，冷读的视图（保留段接在摘要之后）与活会话的视图（存储的先后）建出逐字相同的行：
/// 第三轮的回答在第三轮、排在压缩标记之前。以前冷读把它归到压缩的回合、排在标记之后。
#[test]
fn a_cold_read_and_the_stored_order_give_the_same_rows() {
    let turns = vec![
        TurnRecord {
            turn: TurnId::new(THREE),
            origin: TurnOrigin::Input,
            command: Some(CommandId::new("c3")),
            ..record("msg_u3", Some(InputEnd::Completed))
        },
        record("msg_bd", Some(InputEnd::Completed)),
    ];
    let stored = vec![
        user("msg_u3", "seed three"),
        reply("msg_a3", "three ok"),
        kept("msg_bd", "msg_sum", ("msg_a3", "msg_a3")),
        compact_summary("msg_sum"),
    ];
    let cold = vec![
        stored[0].clone(),
        stored[2].clone(),
        stored[3].clone(),
        stored[1].clone(),
    ];
    let rows = |messages: Vec<Message>| {
        let publisher = built(&view(messages, turns.clone()));
        let mut rows: Vec<Value> = snapshot(&publisher)["collections"][rows::ROWS]
            .as_array()
            .unwrap()
            .clone();
        rows.sort_by_key(|row| row[rows::CREATED_AT_SEQ].as_u64());
        rows
    };
    let hot = rows(stored);
    assert_eq!(rows(cold), hot);
    let answer = hot.iter().position(|row| row["id"] == "msg_a3").unwrap();
    let marker = hot.iter().position(|row| row["id"] == "msg_bd").unwrap();
    assert_eq!(hot[answer]["turnId"], THREE);
    assert!(answer < marker, "{hot:?}");
}
