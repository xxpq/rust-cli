//! 会话 actor 发出的后台工作的开始与结束（K-W1，session-lifecycle.md §10.28）：主线程的工具结果说明了起的后台工作 → 在它的
//! `ToolDone` 之后发 `BackgroundStarted`，视图的 `works` 里有它；终态通知、停止让它结束（`BackgroundEnded`，每项一次）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::agents::{AgentNotice, AgentSignal, AgentSignals};
use rust_cli_kernel_api::background::{BackgroundChange, BackgroundStart};
use rust_cli_kernel_api::env::OperationContext;
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::{AgentId, SessionId, ToolUseId};
use rust_cli_kernel_api::manage::BackgroundKind;
use rust_cli_kernel_api::session_api::{Command, KernelEvent, SessionHandle};
use rust_cli_kernel_api::testing::{
    FakeTool, NoticeEnvironment, ScriptedModel, machine, shell_notice, text_response,
    tool_use_response,
};
use serde_json::json;
use tokio::sync::mpsc;

use crate::test_support::{Setup, envelope, finished, submit, until};

/// actor 的时钟（`Setup` 的缺省）。
const NOW: u64 = 1_000;

/// 调用之后报告起了 `change` 的工具（替 Bash、Agent、TaskStop）。
fn tool(name: &str, change: BackgroundChange) -> Arc<FakeTool> {
    let mut tool = FakeTool::new(name, true, "done");
    tool.output = change.attach(tool.output);
    Arc::new(tool)
}

fn launched(id: &str, kind: BackgroundKind) -> BackgroundChange {
    BackgroundChange::Launched {
        id: id.to_owned(),
        kind,
        description: format!("run {id}"),
        child: None,
    }
}

fn started(event: &KernelEvent, id: &str) -> bool {
    matches!(event, KernelEvent::BackgroundStarted { work } if work.id == id)
}

fn ended(event: &KernelEvent, id: &str) -> bool {
    matches!(event, KernelEvent::BackgroundEnded { work } if work == id)
}

#[tokio::test]
async fn a_launch_in_a_tool_result_starts_a_work_and_its_notice_ends_it() {
    let env = Arc::new(NoticeEnvironment::default());
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Launch", &json!({})),
        text_response(&["started"]),
        text_response(&["noted"]),
    ])));
    setup.set_machine(machine(env.clone()));
    setup.tools = vec![tool("Launch", launched("b1", BackgroundKind::Shell))];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    let seen = until(&mut events, finished).await;
    let done = seen
        .iter()
        .position(|event| matches!(event, KernelEvent::ToolDone { tool_use, .. } if tool_use.as_str() == "toolu_1"))
        .unwrap();
    let start = seen.iter().position(|event| started(event, "b1")).unwrap();
    assert!(done < start, "{seen:?}");
    let expected = BackgroundStart {
        id: "b1".to_owned(),
        kind: BackgroundKind::Shell,
        description: "run b1".to_owned(),
        tool_use: Some(ToolUseId::new("toolu_1")),
        child: None,
        started_at: NOW,
    };
    assert_eq!(built.session.view().await.unwrap().works, [expected]);
    env.finish(shell_notice(1, "b1"));
    let seen = until(&mut events, finished).await;
    assert_eq!(
        seen.iter().filter(|event| ended(event, "b1")).count(),
        1,
        "{seen:?}"
    );
    assert!(built.session.view().await.unwrap().works.is_empty());
}

#[tokio::test]
async fn a_task_stop_of_a_command_ends_its_work() {
    let env = Arc::new(NoticeEnvironment::default());
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Launch", &json!({})),
        tool_use_response("toolu_2", "Stop", &json!({})),
        text_response(&["stopped"]),
    ])));
    setup.set_machine(machine(env));
    setup.tools = vec![
        tool("Launch", launched("b1", BackgroundKind::Shell)),
        tool(
            "Stop",
            BackgroundChange::Stopped {
                id: "b1".to_owned(),
            },
        ),
    ];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    let seen = until(&mut events, finished).await;
    assert!(seen.iter().any(|event| started(event, "b1")), "{seen:?}");
    assert!(seen.iter().any(|event| ended(event, "b1")), "{seen:?}");
    assert!(built.session.view().await.unwrap().works.is_empty());
}

/// 子代理的信号由测试推送；`stop_task` 一律成功。
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
    async fn stop_task(
        &self,
        _session: &SessionId,
        task: &str,
        _operation: OperationContext,
    ) -> Result<String, String> {
        Ok(format!("Successfully stopped task: {task}"))
    }
}

#[tokio::test]
async fn a_user_stop_ends_a_command_now_and_an_agent_at_its_notice() {
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Launch", &json!({})),
        tool_use_response("toolu_2", "Spawn", &json!({})),
        text_response(&["both started"]),
        text_response(&["agent killed"]),
    ])));
    setup.agents = Arc::new(Agents {
        receiver: Mutex::new(Some(receiver)),
    });
    let child = BackgroundChange::Launched {
        id: "agent_1".to_owned(),
        kind: BackgroundKind::Agent,
        description: "Review".to_owned(),
        child: Some(SessionId::new("sess_subagent_agent_1")),
    };
    setup.tools = vec![
        tool("Launch", launched("b1", BackgroundKind::Shell)),
        tool("Spawn", child),
    ];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    until(&mut events, finished).await;
    let works = built.session.view().await.unwrap().works;
    assert_eq!(
        works
            .iter()
            .map(|work| (work.id.as_str(), work.child.as_ref().map(SessionId::as_str)))
            .collect::<Vec<_>>(),
        [("b1", None), ("agent_1", Some("sess_subagent_agent_1"))]
    );
    let stop = |id: &str, task: &str| {
        envelope(
            id,
            Command::StopTask {
                task: task.to_owned(),
            },
        )
    };
    // 后台命令：停了就结束（没有终态通知）
    built.session.send(stop("c2", "b1")).await;
    until(&mut events, |event| ended(event, "b1")).await;
    // 子代理：停的回执之后仍在跑，`killed` 通知进会话时结束
    built.session.send(stop("c3", "agent_1")).await;
    let ids: Vec<String> = built
        .session
        .view()
        .await
        .unwrap()
        .works
        .into_iter()
        .map(|work| work.id)
        .collect();
    assert_eq!(ids, ["agent_1"]);
    sender
        .send(AgentSignal::Notice(AgentNotice {
            seq: 1,
            agent: AgentId::new("agent_1"),
            status: BackgroundStatus::Killed,
            text: "<system-reminder>\nagent killed\n</system-reminder>".to_owned(),
        }))
        .unwrap();
    until(&mut events, |event| ended(event, "agent_1")).await;
    assert!(built.session.view().await.unwrap().works.is_empty());
}
