//! hook 行（protocol-v5.md §49）：v4 的 `hookInvocation` 形状、工具事件的泳道与锚、UserPromptSubmit 拦下时的横幅、退出码 2 拦下的输入的用户行；
//! 由视图建与活会话同值，7 个事件以外不画。

use rust_cli_kernel_api::hook_runs::{
    BlockedPrompt, HookInvocation, HookOrigin, HookRun, HookRunOutcome, HookTool, TurnHooks,
};
use rust_cli_kernel_api::hooks::HookEventName;
use rust_cli_kernel_api::ids::{CommandId, MessageId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::session_api::{
    KernelEvent, Running, SequencedEvent, SessionView, TurnOrigin, TurnOutcome, TurnStart,
};
use rust_cli_kernel_api::store::TurnRecord;
use serde_json::{Value, json};

use super::kit::ENDED;
use super::tests::{live, origin, snapshot, view};
use super::*;

const TURN: &str = "turn_h";
const COMMAND: &str = "cmd_h";
/// 用户消息的 id（`msg_<36 进制毫秒>_…`：行的 `createdAt` 由它读出）。
const PROMPT: &str = "msg_mfqv0000_prompt";
/// 内核交来的那句警告（hooks 的 `prompt_blocked`）。
const BLOCKED_WARNING: &str = "UserPromptSubmit operation blocked by hook:\nhooks_prompt_block";
const STDERR: &str =
    "python3: can't open file '/Users/dev/test/z-m/a.py': [Errno 2] No such file or directory";

fn started() -> KernelEvent {
    KernelEvent::TurnStarted {
        turn: TurnId::new(TURN),
        command: Some(CommandId::new(COMMAND)),
        origin: TurnOrigin::Input,
        started_at: 5,
        works: Vec::new(),
    }
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

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(PROMPT),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn hook(index: usize, command: &str, outcome: HookRunOutcome) -> HookRun {
    HookRun {
        index,
        origin: HookOrigin::User,
        command: command.to_owned(),
        status_message: None,
        outcome,
        reason: None,
        stderr: None,
        started_at: 100 + index as u64,
        ended_at: 120 + index as u64,
    }
}

/// E2E HK-UP-01 的两个 UserPromptSubmit hook：异步的 command 启动后不等，process 以 `continue: false` 拦下、stderr 另写一行。
fn prompt_hooks(blocked: Option<BlockedPrompt>) -> TurnHooks {
    let blocking = HookRun {
        reason: Some("hooks_prompt_block".to_owned()),
        stderr: Some(STDERR.to_owned()),
        ..hook(
            1,
            "/usr/local/bin/node /Users/dev/a/.zcode/hooks-full-e2e-recorder.mjs /Users/dev/a/.zcode/trace.jsonl process",
            HookRunOutcome::Blocked,
        )
    };
    TurnHooks {
        turn: TurnId::new(TURN),
        seq: 0,
        invocation: HookInvocation {
            event: HookEventName::UserPromptSubmit,
            tool: None,
            runs: vec![
                hook(
                    0,
                    "\"/usr/local/bin/node\" \"/Users/dev/a/.zcode/hooks-full-e2e-recorder.mjs\" command-async",
                    HookRunOutcome::Success,
                ),
                blocking,
            ],
        },
        blocked,
    }
}

fn row(publisher: &Publisher, id: &str) -> Value {
    let mut row = json!(publisher.entity(rows::ROWS, id).unwrap());
    row.as_object_mut().unwrap().remove("v");
    row
}

fn control(publisher: &Publisher) -> Value {
    snapshot(publisher)["state"]["control"].clone()
}

/// 修 bug（19 §47.3 R-HOOK-ROWS）：以前 rust 不发 hook 的运行事实，被拦下的 UserPromptSubmit 在界面上没有 hook 行与横幅（HK-UP-01 停在等「Hooks」按钮）。
/// 行与每一项照 v4 的 strict 形状；横幅的文字照 v4，留过这一轮的成功。
#[test]
fn a_blocked_prompt_has_the_v4_hook_row_and_banner() {
    let (_, publisher) = live(vec![
        started(),
        KernelEvent::HooksRan {
            hooks: prompt_hooks(None),
        },
        KernelEvent::MessageDone {
            message: user("E2E_HOOKS_PROMPT_BLOCK: blocked"),
        },
        finished(TurnOutcome::Success),
    ]);
    let id = format!("{TURN}:hooks:0");
    let shown = row(&publisher, &id);
    assert_eq!(shown["kind"], "hookInvocation");
    assert_eq!(shown["turnId"], TURN);
    assert_eq!(shown["stable"], true);
    assert_eq!(shown["hookEventName"], "UserPromptSubmit");
    assert_eq!(shown["lane"], "assistantWork");
    assert_eq!(shown["hookCount"], 2);
    assert_eq!(shown["state"], "completed");
    assert_eq!(
        (&shown["startedAt"], &shown["endedAt"], &shown["durationMs"]),
        (&json!(100.0), &json!(121.0), &json!(21.0))
    );
    assert_eq!(shown.get("anchorToolCallId"), None);
    assert_eq!(
        shown["executions"],
        json!([
            {"hookRunId": format!("{id}:0"), "hookIndex": 0, "didExecute": true, "state": "completed",
             "outcome": "success", "startedAt": 100.0, "endedAt": 120.0, "durationMs": 20.0,
             "displayName": "node · hooks-full-e2e-recorder.mjs", "sourceKind": "user"},
            {"hookRunId": format!("{id}:1"), "hookIndex": 1, "didExecute": true, "state": "completed",
             "outcome": "blocked", "blockReason": "hooks_prompt_block", "startedAt": 101.0, "endedAt": 121.0,
             "durationMs": 20.0, "displayName": "node · hooks-full-e2e-recorder.mjs", "sourceKind": "user"}
        ])
    );
    let control = control(&publisher);
    assert_eq!(control["phase"], "completed");
    let error = &control["lastError"];
    assert_eq!(error["code"], "fault.runtime.hookBlocked");
    assert_eq!(error["message"], format!("hooks_prompt_block: {STDERR}"));
    assert_eq!(
        error["detail"],
        format!("Hook block reason: hooks_prompt_block\nHook error: {STDERR}")
    );
    assert_eq!(
        (&error["recoverable"], &error["source"], &error["at"]),
        (&json!(false), &json!("runtime"), &json!(121.0))
    );
}

/// 下一轮成功结束时横幅换掉（rust 的 `lastError` 在之后的回合结束时清，§15.3.1）。
#[test]
fn a_later_turn_clears_the_banner() {
    let next = KernelEvent::TurnStarted {
        turn: TurnId::new("turn_next"),
        command: None,
        origin: TurnOrigin::Input,
        started_at: 9_500,
        works: Vec::new(),
    };
    let next_finished = KernelEvent::TurnFinished {
        turn: TurnId::new("turn_next"),
        outcome: TurnOutcome::Success,
        usage: None,
        time: ENDED,
        files: Vec::new(),
    };
    let (_, publisher) = live(vec![
        started(),
        KernelEvent::HooksRan {
            hooks: prompt_hooks(None),
        },
        finished(TurnOutcome::Success),
        next,
        next_finished,
    ]);
    assert_eq!(control(&publisher)["lastError"], Value::Null);
}

/// 退出码 2 拦下的输入（照 v4，§47.8 第 6 项）：这条输入没有落库，仍有它的用户行（原文、开回合的命令）；内核的结局是 `PromptBlocked`（§49.8），
/// 回合读成成功，横幅是 hook 拦下（不是回合失败）。
#[test]
fn an_input_blocked_by_exit_two_keeps_its_row_and_the_turn_completes() {
    let blocked = BlockedPrompt {
        message: MessageId::new(PROMPT),
        text: "secret plan".to_owned(),
    };
    let (_, publisher) = live(vec![
        started(),
        KernelEvent::HooksRan {
            hooks: prompt_hooks(Some(blocked)),
        },
        finished(TurnOutcome::PromptBlocked {
            message: BLOCKED_WARNING.to_owned(),
        }),
    ]);
    let input = row(&publisher, PROMPT);
    assert_eq!(input["kind"], "userInput");
    assert_eq!(input["text"], "secret plan");
    assert_eq!(input["turnId"], TURN);
    assert_eq!(input["sourceCommandId"], COMMAND);
    assert_eq!(row(&publisher, TURN)["state"], "completed");
    let control = control(&publisher);
    assert_eq!(control["phase"], "completed");
    assert_eq!(control["lastError"]["code"], "fault.runtime.hookBlocked");
}

/// 修 bug（SMALL-7，§49.8）：以前内核给 `Failed`，投影按活会话里 `HooksRan.blocked` 留下的事实把它改读成成功；topic 在 `HooksRan` 与
/// `TurnFinished` 之间由视图建（新物化、改写历史之后）时没有那条事实，这一轮读成失败，`lastError` 是回合失败的那句。现在内核直接给
/// `PromptBlocked`：由视图建之后到的结局同样读成成功；没有横幅（由视图建的投影本来就没有横幅，§49.2）。
#[test]
fn a_rebuilt_topic_reads_a_blocked_prompt_as_completed() {
    let blocked = BlockedPrompt {
        message: MessageId::new(PROMPT),
        text: "secret plan".to_owned(),
    };
    let mut running = SessionView {
        running: Some(Running::Turn(TurnStart {
            turn: TurnId::new(TURN),
            command: Some(CommandId::new(COMMAND)),
            origin: TurnOrigin::Input,
            started_at: 5,
            works: Vec::new(),
        })),
        ..view(Vec::new(), Vec::new())
    };
    running.hooks = vec![prompt_hooks(Some(blocked))];
    let (mut projection, content) = Projection::build(&running, origin(true));
    let mut publisher = Publisher::new("t", "e");
    publisher.adopt(content);
    let event = SequencedEvent {
        seq: 1,
        event: finished(TurnOutcome::PromptBlocked {
            message: BLOCKED_WARNING.to_owned(),
        }),
        revision: 0,
    };
    let batch = projection.apply(&publisher, &event);
    publisher.publish(batch);
    assert_eq!(row(&publisher, TURN)["state"], "completed");
    let control = control(&publisher);
    assert_eq!(control["phase"], "completed");
    assert_eq!(control["lastError"], Value::Null);
}

fn tool_hooks(seq: usize, event: HookEventName) -> TurnHooks {
    TurnHooks {
        turn: TurnId::new(TURN),
        seq,
        invocation: HookInvocation {
            event,
            tool: Some(HookTool {
                tool_use: ToolUseId::new("toolu_w"),
                name: "Write".to_owned(),
            }),
            runs: vec![HookRun {
                origin: HookOrigin::Plugin {
                    name: "policy".to_owned(),
                },
                status_message: Some(" Checking policy ".to_owned()),
                ..hook(0, "${CLAUDE_PLUGIN_ROOT}/check.sh", HookRunOutcome::Failed)
            }],
        },
        blocked: None,
    }
}

/// 工具事件的行挂在调用旁边（v4 的 `toolBefore` / `toolAfter` 与 `anchorToolCallId`），每项带工具名；插件的项带插件名，显示名先用 `statusMessage`。
#[test]
fn tool_hook_rows_sit_by_their_call() {
    let (_, publisher) = live(vec![
        started(),
        KernelEvent::HooksRan {
            hooks: tool_hooks(0, HookEventName::PreToolUse),
        },
        KernelEvent::HooksRan {
            hooks: tool_hooks(1, HookEventName::PostToolUseFailure),
        },
    ]);
    let before = row(&publisher, &format!("{TURN}:hooks:0"));
    assert_eq!(before["lane"], "toolBefore");
    assert_eq!(before["anchorToolCallId"], "toolu_w");
    assert_eq!(before["state"], "failed");
    let item = &before["executions"][0];
    assert_eq!(
        (
            &item["toolName"],
            &item["sourceKind"],
            &item["pluginName"],
            &item["displayName"],
            &item["state"],
            &item["outcome"],
        ),
        (
            &json!("Write"),
            &json!("plugin"),
            &json!("policy"),
            &json!("Checking policy"),
            &json!("failed"),
            &json!("failed"),
        )
    );
    assert_eq!(item.get("blockReason"), None);
    assert_eq!(
        row(&publisher, &format!("{TURN}:hooks:1"))["lane"],
        "toolAfter"
    );
    // 没有横幅：只有 UserPromptSubmit 的拦下才出
    assert_eq!(control(&publisher)["lastError"], Value::Null);
}

/// 显示名照 v4：插件名接可执行文件；都没有时 `Hook #n`；解释器接脚本的文件名，`-` 开头的参数不算脚本。
#[test]
fn display_names_follow_v4() {
    let named = |origin: HookOrigin, command: &str| {
        hook_rows::display_name(&HookRun {
            origin,
            ..hook(2, command, HookRunOutcome::Success)
        })
    };
    let plugin = || HookOrigin::Plugin {
        name: "policy".to_owned(),
    };
    assert_eq!(named(plugin(), "/opt/check.sh --x"), "policy · check.sh");
    assert_eq!(named(plugin(), ""), "policy");
    assert_eq!(named(HookOrigin::User, ""), "Hook #3");
    assert_eq!(named(HookOrigin::User, "python3 -u x.py"), "python3");
    assert_eq!(
        named(
            HookOrigin::User,
            r#""C:\Program Files\node.exe" "C:\hooks\a b.mjs""#
        ),
        "node.exe · a b.mjs"
    );
    assert_eq!(
        named(HookOrigin::Project, "bash 'scripts/run me.sh'"),
        "bash · run me.sh"
    );
}

/// 由视图建（回合中途订阅、改写历史之后）与活会话同值；没有回合头的回合（被拦下的输入的回合没有回合记录）不放它的行。7 个事件以外不画。
#[test]
fn rows_built_from_the_view_match_the_live_ones() {
    let (_, live_publisher) = live(vec![
        started(),
        KernelEvent::HooksRan {
            hooks: prompt_hooks(None),
        },
        KernelEvent::MessageDone {
            message: user("hello"),
        },
        KernelEvent::HooksRan {
            hooks: tool_hooks(1, HookEventName::PreToolUse),
        },
        finished(TurnOutcome::Success),
    ]);
    let record = TurnRecord {
        turn: TurnId::new(TURN),
        first: MessageId::new(PROMPT),
        origin: TurnOrigin::Input,
        started_at: 5,
        command: Some(CommandId::new(COMMAND)),
        ended: super::kit::ended(
            rust_cli_kernel_api::session_api::InputEnd::Completed,
            Vec::new(),
        ),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let orphan = TurnHooks {
        turn: TurnId::new("turn_blocked"),
        ..prompt_hooks(None)
    };
    let subagent_stop = TurnHooks {
        seq: 2,
        invocation: HookInvocation {
            event: HookEventName::SubagentStop,
            ..prompt_hooks(None).invocation
        },
        ..prompt_hooks(None)
    };
    let mut cold = view(vec![user("hello")], vec![record]);
    cold.hooks = vec![
        prompt_hooks(None),
        tool_hooks(1, HookEventName::PreToolUse),
        orphan,
        subagent_stop,
    ];
    let (_, content) = Projection::build(&cold, origin(true));
    let mut built = Publisher::new("t", "e");
    built.adopt(content);
    for id in [format!("{TURN}:hooks:0"), format!("{TURN}:hooks:1")] {
        let (mut live_row, mut built_row) = (row(&live_publisher, &id), row(&built, &id));
        // 行号是各自的插入顺序（§15.3.4），别的字段同值
        live_row.as_object_mut().unwrap().remove("createdAtSeq");
        built_row.as_object_mut().unwrap().remove("createdAtSeq");
        assert_eq!(live_row, built_row, "{id}");
    }
    assert!(built.entity(rows::ROWS, "turn_blocked:hooks:0").is_none());
    assert!(
        built
            .entity(rows::ROWS, &format!("{TURN}:hooks:2"))
            .is_none()
    );
}

/// 7 个事件以外（例如子代理的 SubagentStop）不画：v4 的行只收这 7 个。
#[test]
fn events_outside_the_v4_seven_are_not_drawn() {
    let hooks = TurnHooks {
        invocation: HookInvocation {
            event: HookEventName::SubagentStop,
            ..prompt_hooks(None).invocation
        },
        ..prompt_hooks(None)
    };
    let (_, publisher) = live(vec![started(), KernelEvent::HooksRan { hooks }]);
    assert!(
        publisher
            .entity(rows::ROWS, &format!("{TURN}:hooks:0"))
            .is_none()
    );
    assert_eq!(control(&publisher)["lastError"], Value::Null);
}
