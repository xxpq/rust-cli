//! 工具行运行中的输出预览（protocol-v5.md §19.4）：带输出的进度写进工具行的 `outputPreview`；有结局（结果、回合结束时取消）就去掉，
//! 活会话与冷读同一组行；子代理的进度不动行。工具行的开工与结束的时刻（§31）：活会话与由视图建同值，回合结束时没结局的行按回合的结束收口。

use rust_cli_kernel_api::ids::{CommandId, MessageId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::metering::ToolTime;
use rust_cli_kernel_api::session_api::{KernelEvent, TurnOrigin, TurnOutcome};
use rust_cli_kernel_api::store::TurnRecord;
use rust_cli_kernel_api::tool::{ToolOutput, ToolProgress};
use serde_json::{Value, json};

use super::super::kit::{apply, live, origin, view};
use super::*;

const TURN: &str = "turn_1";
const CALL: &str = "toolu_bash";
/// 执行器开工与结束的时刻（`ToolStarted` 与随结果落库的 `ToolTime` 同一对）。
const STARTED_AT: u64 = 150;
const ENDED_AT: u64 = 180;

fn time() -> ToolTime {
    ToolTime {
        tool_use: ToolUseId::new(CALL),
        started_at: STARTED_AT,
        ended_at: ENDED_AT,
    }
}

/// 结果提交之后、它的 `MessageDone` 之前内核发的起止（session-lifecycle.md §10.37）。
fn recorded() -> KernelEvent {
    KernelEvent::ToolTimeRecorded { time: time() }
}

fn user() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_u"),
        content: vec![ContentBlock::Text {
            text: "run it".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn call() -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new("msg_a"),
        content: vec![ContentBlock::ToolUse {
            id: ToolUseId::new(CALL),
            name: "Bash".to_owned(),
            input: json!({"command": "make"}),
        }],
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn result() -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_r"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(CALL),
            content: vec![ToolResultContent::Text {
                text: "built".to_owned(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("msg_a"),
        },
    })
}

fn progress(data: Value) -> KernelEvent {
    KernelEvent::ToolProgress {
        tool_use: ToolUseId::new(CALL),
        progress: ToolProgress { data },
    }
}

/// 逐个事件发布，返回这个工具行（去掉版本）。
fn tool_row(
    publisher: &mut Publisher,
    projection: &mut Projection,
    events: Vec<KernelEvent>,
) -> Value {
    apply(publisher, projection, events);
    let mut row = publisher.entity(rows::ROWS, CALL).cloned().unwrap();
    row.remove("v");
    json!(row)
}

fn bash(output: &str, total: u64) -> KernelEvent {
    progress(
        json!({"type": "bash_progress", "output": output, "totalBytes": total, "taskId": "t", "timeoutMs": 1}),
    )
}

/// 回合开始、Bash 调用开始（运行中）。
fn started() -> Vec<KernelEvent> {
    vec![
        KernelEvent::TurnStarted {
            turn: TurnId::new(TURN),
            command: Some(CommandId::new("c1")),
            origin: TurnOrigin::Input,
            started_at: 100,
            works: Vec::new(),
        },
        KernelEvent::MessageDone { message: user() },
        KernelEvent::MessageDone { message: call() },
        KernelEvent::ToolStarted {
            tool_use: ToolUseId::new(CALL),
            name: "Bash".to_owned(),
            input: json!({"command": "make"}),
            started_at: STARTED_AT,
        },
    ]
}

/// 修 bug（19-accept.md §15.3 D5）：以前进度落到 `_ => {}`，工具行没有 `outputPreview`，前台 Bash 运行中看不到输出。
#[test]
fn output_progress_fills_the_running_tool_row() {
    let (mut projection, mut publisher) = live();
    let mut events = started();
    events.push(bash("line-1\n", 7));
    events.push(bash("line-2\n", 14));
    let row = tool_row(&mut publisher, &mut projection, events);
    assert_eq!(row["status"], json!("running"));
    assert_eq!(
        row["outputPreview"],
        json!({"text": "line-1\nline-2\n", "fullText": "line-1\nline-2\n", "totalLines": 3, "totalBytes": 14, "linesEstimated": false})
    );
}

/// 有结局时去掉预览：之后的行与冷读（视图）给出的同一行相同。
#[test]
fn the_outcome_drops_the_preview_like_a_cold_read() {
    let (mut projection, mut publisher) = live();
    let mut events = started();
    events.push(bash("line-1\n", 7));
    events.push(KernelEvent::ToolDone {
        tool_use: ToolUseId::new(CALL),
        output: ToolOutput {
            content: vec![ToolResultContent::Text {
                text: "built".to_owned(),
            }],
            is_error: false,
            display: Value::Null,
        },
        cancelled: false,
    });
    events.push(recorded());
    events.push(KernelEvent::MessageDone { message: result() });
    let live_row = tool_row(&mut publisher, &mut projection, events);
    assert!(live_row.get("outputPreview").is_none(), "{live_row}");
    let record = TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new("msg_u"),
        origin: TurnOrigin::Input,
        started_at: 100,
        command: Some(CommandId::new("c1")),
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let cold_view = rust_cli_kernel_api::session_api::SessionView {
        tool_times: vec![time()],
        ..view(vec![user(), call(), result()], vec![record])
    };
    let (_, content) = Projection::build(&cold_view, origin(false));
    let mut cold = content.collections["rows"][CALL].clone();
    cold.remove("v");
    assert_eq!(live_row, json!(cold));
}

/// 回合结束时还在跑的工具读成「已取消」（02 §11 C7），预览一起去掉。
#[test]
fn a_finished_turn_drops_the_preview_of_a_cancelled_tool() {
    let (mut projection, mut publisher) = live();
    let mut events = started();
    events.push(bash("line-1\n", 7));
    events.push(KernelEvent::TurnFinished {
        turn: TurnId::new(TURN),
        outcome: TurnOutcome::Interrupted,
        usage: None,
        time: super::super::kit::ENDED,
        files: Vec::new(),
    });
    let row = tool_row(&mut publisher, &mut projection, events);
    assert_eq!(row["status"], json!("cancelled"));
    assert!(row.get("outputPreview").is_none(), "{row}");
}

/// 子代理的进度（Agent 工具的 `{agentId, event}`）不是输出：工具行不动。
#[test]
fn subagent_progress_leaves_the_row_alone() {
    let (mut projection, mut publisher) = live();
    let before = tool_row(&mut publisher, &mut projection, started());
    let after = tool_row(
        &mut publisher,
        &mut projection,
        vec![progress(json!({"agentId": "agent_1", "event": {}}))],
    );
    assert_eq!(before, after);
}

/// 修 bug（protocol-v5.md §21，SMALL-FIXES）：大输出的 Bash 结束时，工具行的 `output` 带只给界面的头部（v4 的 `bash_output` display，
/// 放在 `output` 里）；排在后面的结果消息只换文字、不去掉它。冷读没有它（只有结果消息，§21.5 第 1 项）。以前结果行只有给模型的
/// `<persisted-output>`，界面显示了包装与临时文件路径。
#[test]
fn a_persisted_bash_result_carries_the_head_for_the_interface() {
    let persisted = "<persisted-output>\nOutput too large (51.7KB). Full output saved to: /tmp/b.output\n</persisted-output>";
    let (mut projection, mut publisher) = live();
    let mut events = started();
    let output = ToolOutput {
        content: vec![ToolResultContent::Text {
            text: persisted.to_owned(),
        }],
        is_error: false,
        display: Value::Null,
    };
    let head = rust_cli_kernel_api::command_output::CommandOutputHead {
        head: "progress-line-1\nprogress-line-2".to_owned(),
        output_file: "/tmp/b.output".to_owned(),
    };
    events.push(KernelEvent::ToolDone {
        tool_use: ToolUseId::new(CALL),
        output: head.attach(output),
        cancelled: false,
    });
    let displayed = json!({
        "text": persisted,
        "display": {
            "kind": "bash_output",
            "output": "progress-line-1\nprogress-line-2",
            "truncated": true,
            "outputPath": "/tmp/b.output",
        },
    });
    let row = tool_row(&mut publisher, &mut projection, events);
    assert_eq!(row["output"], displayed);
    assert_eq!(row["status"], json!("completed"));
    // 结果消息排在 ToolDone 之后（执行管线先发 ToolFinished 再提交结果）：只补文字，头部留着。
    let row = tool_row(
        &mut publisher,
        &mut projection,
        vec![KernelEvent::MessageDone { message: result() }],
    );
    assert_eq!(row["output"]["display"], displayed["display"]);
    assert_eq!(row["output"]["text"], json!("built"));
    let record = TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new("msg_u"),
        origin: TurnOrigin::Input,
        started_at: 100,
        command: Some(CommandId::new("c1")),
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let (_, content) = Projection::build(
        &view(vec![user(), call(), result()], vec![record]),
        origin(false),
    );
    assert_eq!(
        content.collections["rows"][CALL]["output"],
        json!({"text": "built"})
    );
}

/// 修 bug（protocol-v5.md §26.3，PROJ-GAPS）：由视图建的投影（冷读、激活、改写历史）按视图的 `output_heads` 给结果行补上头部，与活会话的行
/// 相同（成功与失败的结果都是）。以前冷读只有给模型的文字，重新打开的会话显示 `<persisted-output>`。
#[test]
fn a_projection_built_from_the_view_gives_the_stored_head() {
    use rust_cli_kernel_api::command_output::{CommandOutputHead, ToolOutputHead};
    let head = CommandOutputHead {
        head: "progress-line-1".to_owned(),
        output_file: "/tmp/b.output".to_owned(),
    };
    let record = TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new("msg_u"),
        origin: TurnOrigin::Input,
        started_at: 100,
        command: Some(CommandId::new("c1")),
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    for is_error in [false, true] {
        let result = match result() {
            Message::User(mut user) => {
                if let Some(ContentBlock::ToolResult { is_error: e, .. }) = user.content.first_mut()
                {
                    *e = is_error;
                }
                Message::User(user)
            }
            other => other,
        };
        let (mut projection, mut publisher) = live();
        let mut events = started();
        events.push(KernelEvent::ToolDone {
            tool_use: ToolUseId::new(CALL),
            output: ToolOutput {
                is_error,
                ..head
                    .clone()
                    .attach(rust_cli_kernel_api::tool::text("built".to_owned()))
            },
            cancelled: false,
        });
        events.push(recorded());
        events.push(KernelEvent::MessageDone {
            message: result.clone(),
        });
        let live_row = tool_row(&mut publisher, &mut projection, events);
        let cold_view = rust_cli_kernel_api::session_api::SessionView {
            output_heads: vec![ToolOutputHead {
                tool_use: ToolUseId::new(CALL),
                head: head.clone(),
            }],
            tool_times: vec![time()],
            ..view(vec![user(), call(), result], vec![record.clone()])
        };
        let (_, content) = Projection::build(&cold_view, origin(false));
        let mut cold = content.collections["rows"][CALL].clone();
        cold.remove("v");
        assert_eq!(
            cold["output"]["display"]["output"],
            json!("progress-line-1"),
            "is_error = {is_error}"
        );
        assert_eq!(live_row, json!(cold), "is_error = {is_error}");
    }
}

/// 修 bug（protocol-v5.md §31，TOOL-TIME）：工具行带开工与结束的时刻。在跑时先有 `startedAt`（`ToolStarted`），结果落库时 `ToolTimeRecorded` 写两个；
/// 由视图建的投影按视图的 `tool_times`（冷读是 tool part 的 `state.time`）写同一对，与活会话同一行。以前两个都不写。
#[test]
fn the_tool_row_carries_its_start_and_end_live_and_cold() {
    let (mut projection, mut publisher) = live();
    let running = tool_row(&mut publisher, &mut projection, started());
    assert_eq!(running["startedAt"], rows::millis(STARTED_AT));
    assert!(running.get("endedAt").is_none(), "{running}");
    let done = KernelEvent::ToolDone {
        tool_use: ToolUseId::new(CALL),
        output: rust_cli_kernel_api::tool::text("built".to_owned()),
        cancelled: false,
    };
    let live_row = tool_row(
        &mut publisher,
        &mut projection,
        vec![
            done,
            recorded(),
            KernelEvent::MessageDone { message: result() },
        ],
    );
    assert_eq!(
        (&live_row["startedAt"], &live_row["endedAt"]),
        (&rows::millis(STARTED_AT), &rows::millis(ENDED_AT))
    );
    let record = TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new("msg_u"),
        origin: TurnOrigin::Input,
        started_at: 100,
        command: Some(CommandId::new("c1")),
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let cold_view = rust_cli_kernel_api::session_api::SessionView {
        tool_times: vec![time()],
        ..view(vec![user(), call(), result()], vec![record])
    };
    let (_, content) = Projection::build(&cold_view, origin(false));
    let mut cold = content.collections["rows"][CALL].clone();
    cold.remove("v");
    assert_eq!(live_row, json!(cold));
}

/// 回合结束时还没有结局的工具行（02 §11 C7）：已取消，`endedAt` 是回合结束的时刻（照 v4 的 `closeOpenToolRows`）；由视图建时取回合记录里的同一个值。
/// 冷读没有开工的时刻（库里是没有 `time` 的 pending part，§31.5 第 5 项）。没有结束时刻的旧回合记录不写 `endedAt`。
#[test]
fn an_unsettled_tool_ends_with_the_turn_live_and_cold() {
    let (mut projection, mut publisher) = live();
    let mut events = started();
    events.push(KernelEvent::TurnFinished {
        turn: TurnId::new(TURN),
        outcome: TurnOutcome::Interrupted,
        usage: None,
        time: super::super::kit::ENDED,
        files: Vec::new(),
    });
    let live_row = tool_row(&mut publisher, &mut projection, events);
    assert_eq!(live_row["status"], json!("cancelled"));
    assert_eq!(live_row["startedAt"], rows::millis(STARTED_AT));
    assert_eq!(
        live_row["endedAt"],
        rows::millis(super::super::kit::ENDED.ended_at)
    );
    let record = |time| TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new("msg_u"),
        origin: TurnOrigin::Input,
        started_at: 100,
        command: Some(CommandId::new("c1")),
        ended: Some(rust_cli_kernel_api::store::TurnEnded {
            outcome: rust_cli_kernel_api::session_api::InputEnd::Cancelled,
            cancelled: Vec::new(),
            time,
            files: Vec::new(),
        }),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let cold_row = |time| {
        let (_, content) = Projection::build(
            &view(vec![user(), call()], vec![record(time)]),
            origin(false),
        );
        content.collections["rows"][CALL].clone()
    };
    let cold = cold_row(Some(super::super::kit::ENDED));
    assert_eq!(cold["status"], json!("cancelled"));
    assert_eq!(cold["endedAt"], live_row["endedAt"]);
    assert!(!cold.contains_key("startedAt"));
    assert!(!cold_row(None).contains_key("endedAt"));
}

/// 回合中途取的视图（§34）：回合在跑（`TurnStart` 与这一回合的记录），消息是 `messages`，视图另带的两样是 `heads` 与 `cancelled`。
fn mid_turn(
    messages: Vec<Message>,
    heads: Vec<rust_cli_kernel_api::command_output::ToolOutputHead>,
    cancelled: Vec<ToolUseId>,
) -> SessionView {
    let start = rust_cli_kernel_api::session_api::TurnStart {
        turn: TurnId::new(TURN),
        command: Some(CommandId::new("c1")),
        origin: TurnOrigin::Input,
        started_at: 100,
        works: Vec::new(),
    };
    let record = TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new("msg_u"),
        origin: TurnOrigin::Input,
        started_at: 100,
        command: Some(CommandId::new("c1")),
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    SessionView {
        running: Some(rust_cli_kernel_api::session_api::Running::Turn(start)),
        output_heads: heads,
        cancelled,
        ..view(messages, vec![record])
    }
}

fn built_from(view: &SessionView) -> (Projection, Publisher) {
    let (projection, content) = Projection::build(view, origin(true));
    let mut publisher = super::super::kit::publisher();
    publisher.adopt(content);
    (projection, publisher)
}

fn result_of(text: &str, is_error: bool) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_r"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(CALL),
            content: vec![ToolResultContent::Text {
                text: text.to_owned(),
            }],
            is_error,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("msg_a"),
        },
    })
}

/// 修 bug（§34）：视图取在 `ToolDone` 与结果提交之间（流还开着，assistant 与结果都没落库）。以前视图没有头部，结果消息到了只写给模型的文字，
/// 结果行显示 `<persisted-output>`；现在视图带着头部，结果到了按调用 id 取到它。
#[test]
fn a_projection_built_before_the_result_commit_shows_the_output_head() {
    let head = rust_cli_kernel_api::command_output::CommandOutputHead {
        head: "line-1\nline-2".to_owned(),
        output_file: "/tmp/zcode-1000/ses_1/tasks/b1.output".to_owned(),
    };
    let view = mid_turn(
        vec![user()],
        vec![rust_cli_kernel_api::command_output::ToolOutputHead {
            tool_use: ToolUseId::new(CALL),
            head: head.clone(),
        }],
        Vec::new(),
    );
    let (mut projection, mut publisher) = built_from(&view);
    let row = tool_row(
        &mut publisher,
        &mut projection,
        vec![
            KernelEvent::MessageDone { message: call() },
            KernelEvent::MessageDone {
                message: result_of("<persisted-output>", false),
            },
        ],
    );
    assert_eq!(row["status"], json!("completed"));
    assert_eq!(row["output"]["display"], rows::bash_display(head));
}

/// 修 bug（§34）：停止时取消的调用，视图取在回合结束之前。以前回合记录还没有结局，投影把这一行读成「出错」；现在视图带在跑回合里取消的调用：
/// 行已在视图里时建投影就是 cancelled，结果到了不退回；行还不在（流式执行，assistant 还没落库）时结果到了读成 cancelled。
#[test]
fn a_projection_built_before_the_turn_end_keeps_the_stop_cancel() {
    let cancelled = vec![ToolUseId::new(CALL)];
    let (mut projection, mut publisher) = built_from(&mid_turn(
        vec![user(), call()],
        Vec::new(),
        cancelled.clone(),
    ));
    assert_eq!(
        publisher.entity(rows::ROWS, CALL).unwrap()["status"],
        json!("cancelled")
    );
    let rejected = || KernelEvent::MessageDone {
        message: result_of("The user doesn't want to proceed with this tool use.", true),
    };
    let row = tool_row(&mut publisher, &mut projection, vec![rejected()]);
    assert_eq!(row["status"], json!("cancelled"));
    let (mut projection, mut publisher) =
        built_from(&mid_turn(vec![user()], Vec::new(), cancelled));
    let row = tool_row(
        &mut publisher,
        &mut projection,
        vec![KernelEvent::MessageDone { message: call() }, rejected()],
    );
    assert_eq!(row["status"], json!("cancelled"));
}
