//! 子代理行（protocol-v5.md §54.1、§54.3）：`SubagentRecorded` 出行与改行；由视图建与活会话同值；只在库里的按库的结局；视图的优先；
//! 没有 Agent 的工具行就不出行。

use rust_cli_kernel_api::agents::{StoredCall, StoredSubagent};
use rust_cli_kernel_api::ids::{AgentId, CommandId, MessageId, SessionId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock, UserMessage, UserOrigin};
use rust_cli_kernel_api::session_api::{KernelEvent, TurnOrigin};
use rust_cli_kernel_api::store::TurnRecord;
use rust_cli_kernel_api::subagent_runs::{
    SubagentEnded, SubagentOutcome, SubagentRun, SubagentStart,
};
use serde_json::{Value, json};

use super::super::kit::{apply, live, origin, publisher, view};
use super::*;

const TURN: &str = "turn_s";
const CALL: &str = "toolu_agent";
const ROW: &str = "toolu_agent:subagent";
const CHILD: &str = "sess_subagent_agent_1";

fn start(background: bool) -> SubagentStart {
    SubagentStart {
        tool_use: ToolUseId::new(CALL),
        agent: AgentId::new("agent_1"),
        child: SessionId::new(CHILD),
        agent_type: "general-purpose".to_owned(),
        description: "Review".to_owned(),
        background,
    }
}

fn run(background: bool, ended: Option<SubagentOutcome>) -> SubagentRun {
    SubagentRun {
        start: start(background),
        started_at: 1_500,
        ended: ended.map(|outcome| SubagentEnded { outcome, at: 2_500 }),
    }
}

fn prompt() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_u"),
        content: vec![ContentBlock::Text {
            text: "delegate".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

/// 派生子代理的那条 assistant 消息：一次 Agent 调用。
fn spawning() -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new("msg_a"),
        content: vec![ContentBlock::ToolUse {
            id: ToolUseId::new(CALL),
            name: "Agent".to_owned(),
            input: json!({"description": "Review", "prompt": "Look."}),
        }],
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn record() -> TurnRecord {
    TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new("msg_u"),
        origin: TurnOrigin::Input,
        started_at: 1_000,
        command: Some(CommandId::new("c1")),
        ended: super::super::kit::ended(
            rust_cli_kernel_api::session_api::InputEnd::Completed,
            Vec::new(),
        ),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    }
}

/// 回合开了、Agent 的工具行在跑。
fn spawned() -> Vec<KernelEvent> {
    vec![
        KernelEvent::TurnStarted {
            turn: TurnId::new(TURN),
            command: Some(CommandId::new("c1")),
            origin: TurnOrigin::Input,
            started_at: 1_000,
            works: Vec::new(),
        },
        KernelEvent::MessageDone { message: prompt() },
        KernelEvent::MessageDone {
            message: spawning(),
        },
        KernelEvent::ToolStarted {
            tool_use: ToolUseId::new(CALL),
            name: "Agent".to_owned(),
            input: json!({"description": "Review", "prompt": "Look."}),
            started_at: 1_400,
        },
    ]
}

/// 行的内容（去掉 `v`：活会话里它在不同的批里变）。
fn row_of(publisher: &Publisher) -> Option<Value> {
    let mut fields = publisher.entity(ROWS, ROW)?.clone();
    fields.remove("v");
    Some(Value::Object(fields.into_iter().collect()))
}

fn recorded(run: SubagentRun) -> KernelEvent {
    KernelEvent::SubagentRecorded { run }
}

#[test]
fn a_recorded_subagent_is_a_row_on_its_agent_call_s_turn() {
    let (mut projection, mut publisher) = live();
    apply(&mut publisher, &mut projection, spawned());
    apply(
        &mut publisher,
        &mut projection,
        vec![recorded(run(false, None))],
    );
    let row = row_of(&publisher).unwrap();
    let seq = row["createdAtSeq"].clone();
    assert_eq!(
        row,
        json!({
            "kind": "subagent",
            "turnId": TURN,
            "createdAtSeq": seq,
            "stable": false,
            "createdAt": 1_500.0,
            "parentToolCallId": CALL,
            "status": "running",
            "summaryText": "Review",
            "childSessionId": CHILD,
            "subagentType": "general-purpose",
            "startedAt": 1_500.0,
        })
    );
    // 结束：同一行、同一个号
    apply(
        &mut publisher,
        &mut projection,
        vec![recorded(run(false, Some(SubagentOutcome::Failed)))],
    );
    let ended = row_of(&publisher).unwrap();
    assert_eq!(ended["createdAtSeq"], seq);
    assert_eq!(ended["status"], "error");
    assert_eq!(ended["stable"], true);
    assert_eq!(ended["endedAt"], 2_500.0);
    assert!(ended.get("backgrounded").is_none(), "{ended}");
}

#[test]
fn a_background_subagent_row_names_its_work() {
    let (mut projection, mut publisher) = live();
    apply(&mut publisher, &mut projection, spawned());
    apply(
        &mut publisher,
        &mut projection,
        vec![recorded(run(true, Some(SubagentOutcome::Cancelled)))],
    );
    let row = row_of(&publisher).unwrap();
    assert_eq!(row["backgrounded"], true);
    assert_eq!(row["workId"], "agent_1");
    assert_eq!(row["status"], "cancelled");
}

/// 由视图建与活会话同值：两边都只读内核的 `SubagentRun`（行的号可能不同）。
#[test]
fn a_view_builds_the_same_row_as_the_live_session() {
    let (mut projection, mut publisher) = live();
    apply(&mut publisher, &mut projection, spawned());
    let done = run(true, Some(SubagentOutcome::Completed));
    apply(
        &mut publisher,
        &mut projection,
        vec![recorded(done.clone())],
    );
    let mut from_view = view(vec![prompt(), spawning()], vec![record()]);
    from_view.subagents = vec![done];
    let (_, content) = Projection::build(&from_view, origin(true));
    let built = publisher_with(content);
    let without_seq = |mut row: Value| {
        row.as_object_mut().unwrap().remove("createdAtSeq");
        row
    };
    assert_eq!(
        without_seq(row_of(&built).unwrap()),
        without_seq(row_of(&publisher).unwrap())
    );
    // 由视图建时排在派生它的那条 assistant 消息的行之后
    let tool_seq = built.entity(ROWS, CALL).unwrap()["createdAtSeq"].clone();
    let row = row_of(&built).unwrap();
    assert!(row["createdAtSeq"].as_u64() > tool_seq.as_u64(), "{row}");
}

fn publisher_with(content: Content) -> Publisher {
    let mut built = publisher();
    built.adopt(content);
    built
}

fn stored(call: StoredCall) -> StoredSubagent {
    StoredSubagent {
        tool_use: ToolUseId::new(CALL),
        child: SessionId::new(CHILD),
        agent: Some(AgentId::new("agent_1")),
        agent_type: None,
        description: Some("Review".to_owned()),
        call,
        started_at_ms: Some(1_400),
        ended_at_ms: Some(2_000),
        summary: Some("found it".to_owned()),
    }
}

fn cold(subagents: Vec<StoredSubagent>, live_runs: Vec<SubagentRun>) -> Option<Value> {
    let mut from_view = view(vec![prompt(), spawning()], vec![record()]);
    from_view.subagents = live_runs;
    let mut source = origin(false);
    source.subagents = subagents;
    let (_, content) = Projection::build(&from_view, source);
    row_of(&publisher_with(content))
}

/// 只在库里的（冷会话、本进程之前的、Node 写的）：结局按调用；没有类型时 `subagent`；不写 `backgrounded`。
#[test]
fn a_stored_subagent_is_a_row_with_the_call_s_outcome() {
    let row = cold(vec![stored(StoredCall::Completed)], Vec::new()).unwrap();
    assert_eq!(row["status"], "completed");
    assert_eq!(row["subagentType"], "subagent");
    assert_eq!(row["summaryText"], "Review");
    assert_eq!(row["childSessionId"], CHILD);
    assert_eq!(row["parentToolCallId"], CALL);
    assert_eq!(row["startedAt"], 1_400.0);
    assert_eq!(row["endedAt"], 2_000.0);
    assert!(row.get("backgrounded").is_none(), "{row}");
    let failed = |error: &str| {
        cold(
            vec![stored(StoredCall::Failed {
                error: error.to_owned(),
            })],
            Vec::new(),
        )
        .unwrap()["status"]
            .clone()
    };
    assert_eq!(failed("Subagent was stopped by user"), "cancelled");
    assert_eq!(failed("429 too many requests"), "error");
    let unfinished = cold(vec![stored(StoredCall::Unfinished)], Vec::new()).unwrap();
    assert_eq!(unfinished["status"], "cancelled");
}

/// 本进程派生、结果也已落库的：视图的那一项优先（库里推出来的结局不换掉内核记下的）。
#[test]
fn the_view_s_subagent_wins_over_the_stored_one() {
    let row = cold(
        vec![stored(StoredCall::Completed)],
        vec![run(true, Some(SubagentOutcome::Failed))],
    )
    .unwrap();
    assert_eq!(row["status"], "error");
    assert_eq!(row["backgrounded"], true);
}

/// 行要配 Agent 的工具行：改写历史截掉了派生它的调用（视图里没有那条消息），就不出这一行；活会话里同样。
#[test]
fn no_agent_call_row_means_no_subagent_row() {
    let mut from_view = view(vec![prompt()], vec![record()]);
    from_view.subagents = vec![run(false, Some(SubagentOutcome::Completed))];
    let (_, content) = Projection::build(&from_view, origin(true));
    assert_eq!(row_of(&publisher_with(content)), None);
    let (mut projection, mut publisher) = live();
    apply(
        &mut publisher,
        &mut projection,
        vec![recorded(run(false, None))],
    );
    assert_eq!(row_of(&publisher), None);
}
