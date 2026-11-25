//! 回合头的文件改动与 ExitPlanMode 行的计划（protocol-v5.md §42.1）：活会话取事件（`TurnFinished.files`、`ToolDone` 的 display），由视图建时取回合记录与视图的
//! `plans`，同一个函数、同一个值；没有改动的回合头不写这两个字段。

use rust_cli_kernel_api::ids::{CommandId, MachineName, MessageId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::plan::ShownPlan;
use rust_cli_kernel_api::session_api::{
    FileChange, InputEnd, KernelEvent, TurnFileChange, TurnOrigin, TurnOutcome,
};
use rust_cli_kernel_api::store::{TurnEnded, TurnRecord};
use rust_cli_kernel_api::tool::{ToolOutput, text};
use rust_cli_kernel_api::tool_names::EXIT_PLAN_MODE;
use serde_json::{Value, json};

use super::super::kit::{ENDED, apply, live, origin, publisher, view};
use super::*;

const TURN: &str = "turn_1";
const CALL: &str = "toolu_plan";
const PLAN: &str = "# Plan\n1. do it";
const PATH: &str = "/Users/dev/a/.zcode/plans/plan-ses_1.md";

fn file(path: &str, additions: u64, deletions: u64, tools: &[&str]) -> TurnFileChange {
    TurnFileChange {
        change: FileChange {
            path: path.to_owned(),
            additions,
            deletions,
            machine: MachineName::primary(),
        },
        writes: tools.len() as u64,
        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
    }
}

fn files() -> Vec<TurnFileChange> {
    vec![
        file("/Users/dev/a/a.txt", 2, 1, &["Edit", "Write"]),
        file("/Users/dev/a/b.txt", 1, 0, &["Write"]),
    ]
}

fn user(id: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "go".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn exit_plan(id: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolUse {
            id: ToolUseId::new(CALL),
            name: EXIT_PLAN_MODE.to_owned(),
            input: json!({}),
        }],
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn result(id: &str, owner: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(CALL),
            content: vec![ToolResultContent::Text {
                text: "approved".to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new(owner),
        },
    })
}

fn messages() -> Vec<Message> {
    vec![
        user("msg_u1"),
        exit_plan("msg_a1"),
        result("msg_r1", "msg_a1"),
    ]
}

fn shown() -> ShownPlan {
    ShownPlan {
        tool_use: ToolUseId::new(CALL),
        plan: PLAN.to_owned(),
        path: PATH.to_owned(),
    }
}

fn output() -> ToolOutput {
    ShownPlan::attach(
        PLAN.to_owned(),
        PATH.to_owned(),
        text("approved".to_owned()),
    )
}

fn record(files: Vec<TurnFileChange>) -> TurnRecord {
    TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new("msg_u1"),
        origin: TurnOrigin::Input,
        started_at: 100,
        command: Some(CommandId::new("c1")),
        ended: Some(TurnEnded {
            outcome: InputEnd::Completed,
            cancelled: Vec::new(),
            time: Some(ENDED),
            files,
        }),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    }
}

/// 活会话：一个回合逐个事件发布；`plan_first` 时 ExitPlanMode 的 `ToolDone` 排在它的 assistant 的 `MessageDone` 之前（流式执行）。
fn followed(files: Vec<TurnFileChange>, plan_first: bool) -> Publisher {
    let (mut projection, mut publisher) = live();
    let done = KernelEvent::ToolDone {
        tool_use: ToolUseId::new(CALL),
        output: output(),
        cancelled: false,
    };
    let mut events = vec![
        KernelEvent::TurnStarted {
            turn: TurnId::new(TURN),
            command: Some(CommandId::new("c1")),
            origin: TurnOrigin::Input,
            started_at: 100,
            works: Vec::new(),
        },
        KernelEvent::MessageDone {
            message: user("msg_u1"),
        },
    ];
    let assistant = KernelEvent::MessageDone {
        message: exit_plan("msg_a1"),
    };
    if plan_first {
        events.extend([done, assistant]);
    } else {
        events.extend([assistant, done]);
    }
    events.extend([
        KernelEvent::MessageDone {
            message: result("msg_r1", "msg_a1"),
        },
        KernelEvent::TurnFinished {
            turn: TurnId::new(TURN),
            outcome: TurnOutcome::Success,
            usage: None,
            time: ENDED,
            files,
        },
    ]);
    apply(&mut publisher, &mut projection, events);
    publisher
}

/// 冷读：视图的消息、回合记录与 `plans`。
fn cold(files: Vec<TurnFileChange>) -> Publisher {
    let mut view = view(messages(), vec![record(files)]);
    view.plans = vec![shown()];
    let (_, content) = Projection::build(&view, origin(false));
    let mut cold = publisher();
    cold.adopt(content);
    cold
}

fn field(publisher: &Publisher, id: &str, key: &str) -> Value {
    publisher
        .entity(rows::ROWS, id)
        .unwrap()
        .get(key)
        .cloned()
        .unwrap_or(Value::Null)
}

/// 以前回合头不写文件改动，界面的文件改动面板在开关开时从不出现；现在回合结束时写汇总与每个文件一项，冷读同值。
#[test]
fn the_turn_header_has_the_turn_file_changes_live_and_cold() {
    let live = followed(files(), false);
    assert_eq!(
        field(&live, TURN, "fileChangeSummary"),
        json!({"additions": 3.0, "deletions": 1.0, "files": 2.0, "state": "active"})
    );
    assert_eq!(
        field(&live, TURN, "fileChanges"),
        json!([
            {"path": "/Users/dev/a/a.txt", "additions": 2, "deletions": 1, "writeCount": 2, "toolNames": ["Edit", "Write"], "patches": []},
            {"path": "/Users/dev/a/b.txt", "additions": 1, "deletions": 0, "writeCount": 1, "toolNames": ["Write"], "patches": []},
        ])
    );
    let cold = cold(files());
    for key in ["fileChangeSummary", "fileChanges"] {
        assert_eq!(field(&cold, TURN, key), field(&live, TURN, key), "{key}");
    }
}

/// 没有改动的回合头不写这两个字段（同 v4：文件数为 0 时不写；界面按没有汇总不画面板）。
#[test]
fn a_turn_without_file_changes_writes_neither_field() {
    for publisher in [followed(Vec::new(), false), cold(Vec::new())] {
        let header = publisher.entity(rows::ROWS, TURN).unwrap();
        assert!(!header.contains_key("fileChangeSummary"), "{header:?}");
        assert!(!header.contains_key("fileChanges"), "{header:?}");
    }
}

/// 以前 ExitPlanMode 行的 `input` 只有模型的参数（`{}`），界面的计划目录与计划卡只能从结果文字里取；现在带工具读到的计划与计划文件，冷读同值，
/// `inputText` 仍是模型的原始输入。
#[test]
fn the_exit_plan_mode_row_carries_the_shown_plan_live_and_cold() {
    let live = followed(Vec::new(), false);
    assert_eq!(
        field(&live, CALL, "input"),
        json!({"plan": PLAN, "planFilePath": PATH})
    );
    assert_eq!(field(&live, CALL, "inputText"), json!("{}"));
    let cold = cold(Vec::new());
    for key in ["input", "inputText", "status"] {
        assert_eq!(field(&cold, CALL, key), field(&live, CALL, key), "{key}");
    }
}

/// 流式执行时 assistant 的 `MessageDone` 晚于 `ToolDone`：重写这一行时按记下的计划再带上，不丢。
#[test]
fn a_plan_shown_before_its_assistant_message_stays_on_the_row() {
    let live = followed(Vec::new(), true);
    assert_eq!(
        field(&live, CALL, "input"),
        json!({"plan": PLAN, "planFilePath": PATH})
    );
}

/// 撤销过文件改动的回合（§53.5，同 v4 的 `fileChanges.state = reverted`）：活会话的 `FilesRestored` 与由视图建时视图的 `reverted` 给同一个状态，汇总的数字不变；
/// 没有文件改动的回合头不动。
#[test]
fn a_restored_turn_reads_reverted_live_and_cold() {
    let mut view = view(messages(), vec![record(files())]);
    let (mut projection, content) = Projection::build(&view, origin(true));
    let mut live = publisher();
    live.adopt(content);
    apply(
        &mut live,
        &mut projection,
        vec![KernelEvent::FilesRestored {
            prompts: vec![MessageId::new("msg_u1")],
        }],
    );
    let reverted = json!({"additions": 3.0, "deletions": 1.0, "files": 2.0, "state": "reverted"});
    assert_eq!(field(&live, TURN, "fileChangeSummary"), reverted);
    view.reverted = vec![MessageId::new("msg_u1")];
    let (_, content) = Projection::build(&view, origin(false));
    let mut cold = publisher();
    cold.adopt(content);
    assert_eq!(field(&cold, TURN, "fileChangeSummary"), reverted);
    assert_eq!(
        field(&cold, TURN, "fileChanges"),
        field(&live, TURN, "fileChanges")
    );
    // 没有文件改动的回合头：不写汇总
    let mut plain = super::super::kit::view(messages(), vec![record(Vec::new())]);
    plain.reverted = vec![MessageId::new("msg_u1")];
    let (_, content) = Projection::build(&plain, origin(false));
    let mut none = publisher();
    none.adopt(content);
    assert_eq!(field(&none, TURN, "fileChangeSummary"), Value::Null);
}
