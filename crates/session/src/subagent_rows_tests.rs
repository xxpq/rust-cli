//! 会话 actor 的子代理表（session-lifecycle.md §10.56）：Agent 工具报开始之后在 `ToolDone` 之前有一项在跑的；前台的结局看这次调用的结果
//! （完成、失败、被停）；停放与续跑跟着后台工作表收下的开始转后台、再跑；后台的结局看终态通知；视图带着表里的全部。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::agents::{AgentNotice, AgentSignal, AgentSignals};
use rust_cli_kernel_api::background::BackgroundChange;
use rust_cli_kernel_api::classifier::ClassifierInput;
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::{AgentId, SessionId, ToolUseId};
use rust_cli_kernel_api::manage::BackgroundKind;
use rust_cli_kernel_api::session_api::{Command, KernelEvent, SessionHandle};
use rust_cli_kernel_api::subagent_runs::{
    SubagentEnded, SubagentOutcome, SubagentRun, SubagentStart,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use super::*;
use crate::test_support::{Setup, envelope, finished, submit, until};

/// actor 的时钟（`Setup` 的缺省）。
const NOW: u64 = 1_000;
const AGENT: &str = "agent_1";
const CHILD: &str = "sess_subagent_agent_1";

fn start(tool_use: &str, background: bool) -> SubagentStart {
    SubagentStart {
        tool_use: ToolUseId::new(tool_use),
        agent: AgentId::new(AGENT),
        child: SessionId::new(CHILD),
        agent_type: "general-purpose".to_owned(),
        description: "Review".to_owned(),
        background,
    }
}

fn launched() -> BackgroundChange {
    BackgroundChange::Launched {
        id: AGENT.to_owned(),
        kind: BackgroundKind::Agent,
        description: "Review".to_owned(),
        child: Some(SessionId::new(CHILD)),
    }
}

fn text(text: &str) -> ToolOutput {
    rust_cli_kernel_api::tool::text(text.to_owned())
}

/// 替 Agent：报一次开始（`start` 有值时），之后等 `gate`（有值时；被停回取消），再交出 `output`。
struct Spawn {
    name: &'static str,
    start: Option<SubagentStart>,
    gate: Option<Arc<Notify>>,
    output: ToolOutput,
}

impl Spawn {
    fn new(name: &'static str, start: Option<SubagentStart>, output: ToolOutput) -> Self {
        Self {
            name,
            start,
            gate: None,
            output,
        }
    }
}

#[async_trait]
impl Tool for Spawn {
    fn classifier_input(&self, _input: &Value) -> ClassifierInput {
        ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        self.name
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        format!("{} test tool", self.name)
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    async fn call(
        &self,
        _input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        if let Some(start) = &self.start {
            context.session_ops.subagent_started(start.clone()).await;
        }
        if let Some(gate) = &self.gate {
            tokio::select! {
                () = gate.notified() => {}
                () = context.cancel.cancelled() => return Err(ToolError::Cancelled),
            }
        }
        Ok(self.output.clone())
    }
}

/// 子代理的信号由测试推送。
struct Agents {
    receiver: Mutex<Option<mpsc::UnboundedReceiver<AgentSignal>>>,
}

#[async_trait]
impl AgentSignals for Agents {
    fn signals(&self, _session: &SessionId) -> BoxStream<'static, AgentSignal> {
        let receiver = self.receiver.lock().unwrap().take().unwrap();
        stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|signal| (signal, receiver))
        })
        .boxed()
    }
    async fn ack(&self, _session: &SessionId, _seq: u64) {}
    async fn background(&self, _session: &SessionId) -> Vec<AgentId> {
        Vec::new()
    }
}

fn notice(seq: u64, status: BackgroundStatus) -> AgentSignal {
    AgentSignal::Notice(AgentNotice {
        seq,
        agent: AgentId::new(AGENT),
        status,
        text: format!(
            "<system-reminder>\nagent {}\n</system-reminder>",
            status.as_str()
        ),
    })
}

fn recorded(event: &KernelEvent) -> Option<&SubagentRun> {
    match event {
        KernelEvent::SubagentRecorded { run } => Some(run),
        _ => None,
    }
}

fn ended_as(outcome: SubagentOutcome) -> impl Fn(&KernelEvent) -> bool {
    move |event| {
        recorded(event).is_some_and(|run| run.ended.map(|ended| ended.outcome) == Some(outcome))
    }
}

fn position(seen: &[KernelEvent], found: impl Fn(&KernelEvent) -> bool) -> usize {
    seen.iter()
        .position(found)
        .unwrap_or_else(|| panic!("not found in {seen:?}"))
}

fn tool_done(event: &KernelEvent) -> bool {
    matches!(event, KernelEvent::ToolDone { tool_use, .. } if tool_use.as_str() == "toolu_1")
}

#[tokio::test]
async fn a_foreground_subagent_starts_before_its_tool_result_and_ends_after_it() {
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Agent", &json!({})),
        text_response(&["done"]),
    ])));
    setup.tools = vec![Arc::new(Spawn::new(
        "Agent",
        Some(start("toolu_1", false)),
        text("handed back"),
    ))];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    let seen = until(&mut events, finished).await;
    let started = position(
        &seen,
        |event| matches!(event, KernelEvent::ToolStarted { tool_use, .. } if tool_use.as_str() == "toolu_1"),
    );
    let running = position(&seen, |event| {
        recorded(event).is_some_and(|run| run.ended.is_none())
    });
    let done = position(&seen, tool_done);
    let ended = position(&seen, ended_as(SubagentOutcome::Completed));
    let turn = position(&seen, finished);
    assert!(
        started < running && running < done && done < ended && ended < turn,
        "{seen:?}"
    );
    let expected = SubagentRun {
        start: start("toolu_1", false),
        started_at: NOW,
        ended: Some(SubagentEnded {
            outcome: SubagentOutcome::Completed,
            at: NOW,
        }),
    };
    assert_eq!(built.session.view().await.unwrap().subagents, [expected]);
}

#[tokio::test]
async fn a_failed_foreground_call_ends_its_subagent_as_failed() {
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Agent", &json!({})),
        text_response(&["done"]),
    ])));
    let mut failed = text("<tool_use_error>store failed</tool_use_error>");
    failed.is_error = true;
    setup.tools = vec![Arc::new(Spawn::new(
        "Agent",
        Some(start("toolu_1", false)),
        failed,
    ))];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    let seen = until(&mut events, finished).await;
    assert!(
        seen.iter().any(ended_as(SubagentOutcome::Failed)),
        "{seen:?}"
    );
}

#[tokio::test]
async fn stopping_the_turn_cancels_a_foreground_subagent() {
    let gate = Arc::new(Notify::new());
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![tool_use_response(
        "toolu_1",
        "Agent",
        &json!({}),
    )])));
    let mut spawn = Spawn::new("Agent", Some(start("toolu_1", false)), text("unused"));
    spawn.gate = Some(gate);
    setup.tools = vec![Arc::new(spawn)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    until(&mut events, |event| recorded(event).is_some()).await;
    built
        .session
        .send(envelope("c2", Command::Interrupt { target: None }))
        .await;
    let seen = until(&mut events, finished).await;
    let done = position(&seen, |event| {
        matches!(
            event,
            KernelEvent::ToolDone {
                cancelled: true,
                ..
            }
        )
    });
    assert!(
        done < position(&seen, ended_as(SubagentOutcome::Cancelled)),
        "{seen:?}"
    );
}

#[tokio::test]
async fn a_background_subagent_ends_at_its_notice() {
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Agent", &json!({})),
        text_response(&["started"]),
        text_response(&["noted"]),
    ])));
    setup.agents = Arc::new(Agents {
        receiver: Mutex::new(Some(receiver)),
    });
    setup.tools = vec![Arc::new(Spawn::new(
        "Agent",
        Some(start("toolu_1", true)),
        launched().attach(text("launched")),
    ))];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    let seen = until(&mut events, finished).await;
    // 在后台跑：工具结束不结束它，只发一次开始
    assert_eq!(seen.iter().filter_map(recorded).count(), 1, "{seen:?}");
    let view = built.session.view().await.unwrap();
    assert!(view.subagents[0].start.background && view.subagents[0].ended.is_none());
    sender.send(notice(1, BackgroundStatus::Failed)).unwrap();
    until(&mut events, ended_as(SubagentOutcome::Failed)).await;
    let ended = built.session.view().await.unwrap().subagents[0].ended;
    assert_eq!(
        ended,
        Some(SubagentEnded {
            outcome: SubagentOutcome::Failed,
            at: NOW,
        })
    );
}

#[tokio::test]
async fn a_parked_foreground_subagent_turns_background_and_ends_at_its_notice() {
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Agent", &json!({})),
        text_response(&["parked"]),
        text_response(&["noted"]),
    ])));
    setup.agents = Arc::new(Agents {
        receiver: Mutex::new(Some(receiver)),
    });
    // 前台派生，跑完还有在跑的后台子代理：回「已启动」，结果带 Launched（agents.md §5.1）
    setup.tools = vec![Arc::new(Spawn::new(
        "Agent",
        Some(start("toolu_1", false)),
        launched().attach(text("launched")),
    ))];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    let seen = until(&mut events, finished).await;
    let parked = seen
        .iter()
        .filter_map(recorded)
        .next_back()
        .unwrap_or_else(|| panic!("{seen:?}"));
    assert!(
        parked.start.background && parked.ended.is_none(),
        "{seen:?}"
    );
    sender.send(notice(1, BackgroundStatus::Killed)).unwrap();
    until(&mut events, ended_as(SubagentOutcome::Cancelled)).await;
}

/// 后台子代理在它的工具结束之前就结束了：通知先结束它，之后的 Launched 被后台工作表丢掉（§10.28），不再「再跑」。
#[tokio::test]
async fn a_notice_that_beats_the_tool_result_keeps_the_subagent_ended() {
    let (sender, receiver) = mpsc::unbounded_channel();
    let gate = Arc::new(Notify::new());
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Agent", &json!({})),
        text_response(&["started"]),
        text_response(&["noted"]),
    ])));
    setup.agents = Arc::new(Agents {
        receiver: Mutex::new(Some(receiver)),
    });
    let mut spawn = Spawn::new(
        "Agent",
        Some(start("toolu_1", true)),
        launched().attach(text("launched")),
    );
    spawn.gate = Some(gate.clone());
    setup.tools = vec![Arc::new(spawn)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    until(&mut events, |event| recorded(event).is_some()).await;
    sender.send(notice(1, BackgroundStatus::Completed)).unwrap();
    until(&mut events, ended_as(SubagentOutcome::Completed)).await;
    gate.notify_one();
    let seen = until(&mut events, finished).await;
    assert_eq!(seen.iter().filter_map(recorded).count(), 0, "{seen:?}");
    let view = built.session.view().await.unwrap();
    assert!(view.subagents[0].ended.is_some(), "{view:?}");
    assert!(view.works.is_empty(), "{:?}", view.works);
}

/// SendMessage 续跑一个结束了的子代理：它的工具结果带同一个子会话的 Launched，那一项回到在跑（开始的时刻换新）。
#[tokio::test]
async fn a_resumed_subagent_runs_again() {
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Agent", &json!({})),
        text_response(&["started"]),
        text_response(&["noted"]),
        tool_use_response("toolu_2", "SendMessage", &json!({})),
        text_response(&["resumed"]),
    ])));
    setup.agents = Arc::new(Agents {
        receiver: Mutex::new(Some(receiver)),
    });
    setup.tools = vec![
        Arc::new(Spawn::new(
            "Agent",
            Some(start("toolu_1", true)),
            launched().attach(text("launched")),
        )),
        Arc::new(Spawn::new(
            "SendMessage",
            None,
            launched().attach(text("resumed")),
        )),
    ];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    until(&mut events, finished).await;
    sender.send(notice(1, BackgroundStatus::Completed)).unwrap();
    until(&mut events, finished).await;
    built.session.send(submit("c2", "again")).await;
    let seen = until(&mut events, finished).await;
    let again = seen
        .iter()
        .filter_map(recorded)
        .next_back()
        .unwrap_or_else(|| panic!("{seen:?}"));
    assert!(again.ended.is_none() && again.start.background, "{seen:?}");
    // 还是那一项：派生它的调用不变
    assert_eq!(again.start.tool_use.as_str(), "toolu_1");
    assert_eq!(built.session.view().await.unwrap().subagents.len(), 1);
}

#[test]
fn the_table_only_changes_the_entry_that_is_still_running() {
    let mut rows = SubagentRows::default();
    rows.started(start("toolu_1", false), 1);
    // 不是它派生的调用、不在表里的子会话、不认识的子代理：不动
    assert_eq!(
        rows.foreground_ended(&ToolUseId::new("toolu_9"), SubagentOutcome::Completed, 2),
        None
    );
    assert_eq!(rows.launched(&SessionId::new("sess_other"), 2), None);
    assert_eq!(
        rows.noticed(&AgentId::new("agent_9"), SubagentOutcome::Completed, 2),
        None
    );
    let ended = rows.foreground_ended(&ToolUseId::new("toolu_1"), SubagentOutcome::Completed, 3);
    assert_eq!(
        ended.and_then(|run| run.ended).map(|ended| ended.at),
        Some(3)
    );
    // 结束了的不再结束
    assert_eq!(
        rows.foreground_ended(&ToolUseId::new("toolu_1"), SubagentOutcome::Failed, 4),
        None
    );
    assert_eq!(
        rows.noticed(&AgentId::new(AGENT), SubagentOutcome::Failed, 4),
        None
    );
    // 在后台跑的再收到开始：不动
    let mut background = SubagentRows::default();
    background.started(start("toolu_1", true), 1);
    assert_eq!(background.launched(&SessionId::new(CHILD), 2), None);
}

#[test]
fn a_notice_without_a_terminal_status_has_no_outcome() {
    assert_eq!(noticed_outcome(BackgroundStatus::Running), None);
    assert_eq!(
        noticed_outcome(BackgroundStatus::Killed),
        Some(SubagentOutcome::Cancelled)
    );
}
