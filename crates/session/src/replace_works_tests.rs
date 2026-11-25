//! 换掉一条输入时停被截掉的回合里起的后台工作（session-lifecycle.md §10.55）：名单由会话 actor 在截断的同一步按自己的记录定；回合在跑时它中止之后才起的
//! 也在名单里；被停的子代理的 killed 通知不进新分支。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::agents::{AgentNotice, AgentSignal, AgentSignals};
use rust_cli_kernel_api::background::BackgroundChange;
use rust_cli_kernel_api::classifier::ClassifierInput;
use rust_cli_kernel_api::env::OperationContext;
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::{AgentId, MessageId, SessionId};
use rust_cli_kernel_api::manage::BackgroundKind;
use rust_cli_kernel_api::message::{Message, UserOrigin};
use rust_cli_kernel_api::session_api::{Ack, Command, CommandResult, KernelEvent, SessionHandle};
use rust_cli_kernel_api::testing::{FakeTool, ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use crate::test_support::{Setup, envelope, finished, submit, until};

/// 子代理的信号由测试推送；记下停了哪些、ack 了哪些（`stop_task` 一律成功）。
#[derive(Default)]
struct Agents {
    receiver: Mutex<Option<mpsc::UnboundedReceiver<AgentSignal>>>,
    stopped: Mutex<Vec<String>>,
    acked: Mutex<Vec<u64>>,
}

#[async_trait]
impl AgentSignals for Agents {
    fn signals(&self, _session: &SessionId) -> BoxStream<'static, AgentSignal> {
        match self.receiver.lock().unwrap().take() {
            Some(receiver) => stream::unfold(receiver, |mut receiver| async move {
                receiver.recv().await.map(|signal| (signal, receiver))
            })
            .boxed(),
            None => stream::pending().boxed(),
        }
    }
    async fn ack(&self, _session: &SessionId, seq: u64) {
        self.acked.lock().unwrap().push(seq);
    }
    async fn background(&self, _session: &SessionId) -> Vec<AgentId> {
        Vec::new()
    }
    async fn stop_task(
        &self,
        _session: &SessionId,
        task: &str,
        _operation: OperationContext,
    ) -> Result<String, String> {
        self.stopped.lock().unwrap().push(task.to_owned());
        Ok(format!("Successfully stopped task: {task}"))
    }
}

fn launched(id: &str, kind: BackgroundKind) -> BackgroundChange {
    BackgroundChange::Launched {
        id: id.to_owned(),
        kind,
        description: format!("run {id}"),
        child: None,
    }
}

/// 调用之后报告起了 `change` 的工具。
fn launcher(name: &str, change: BackgroundChange) -> Arc<FakeTool> {
    let mut tool = FakeTool::new(name, true, "done");
    tool.output = change.attach(tool.output);
    Arc::new(tool)
}

/// 不理会中止、等闸门放行才结束的工具（后台命令已经起了、工具还在收尾）：结束时报告起了 `change`。
struct Stubborn {
    started: Arc<Notify>,
    gate: Arc<Notify>,
    output: ToolOutput,
}

#[async_trait]
impl Tool for Stubborn {
    fn name(&self) -> &str {
        "Slow"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &PromptContext) -> String {
        "slow test tool".to_owned()
    }
    fn classifier_input(&self, _input: &Value) -> ClassifierInput {
        ClassifierInput::Skip
    }
    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        self.started.notify_one();
        self.gate.notified().await;
        Ok(self.output.clone())
    }
}

fn replace(target: &MessageId, text: &str) -> Command {
    Command::ReplaceInput {
        target: target.clone(),
        text: text.to_owned(),
        extras: Default::default(),
        restore_code: false,
        stop_cut_works: true,
    }
}

fn prompts(messages: &[Message]) -> Vec<MessageId> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) if user.origin == UserOrigin::Human => Some(user.id.clone()),
            _ => None,
        })
        .collect()
}

fn work_ids(works: &[rust_cli_kernel_api::background::BackgroundStart]) -> Vec<&str> {
    works.iter().map(|work| work.id.as_str()).collect()
}

/// 回合在跑时编辑：工作在编辑到了之后才起（不理会中止的工具收尾时报告的），也在名单里被停；更早的回合里起的不停。
#[tokio::test]
async fn an_edit_during_a_turn_also_stops_a_work_the_turn_starts_after_it() {
    let agents = Arc::new(Agents::default());
    let (started, gate) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_keep", "Keep", &json!({})),
        text_response(&["one"]),
        tool_use_response("toolu_slow", "Slow", &json!({})),
        text_response(&["edited"]),
    ]));
    let mut setup = Setup::new(model.clone());
    setup.agents = agents.clone();
    let slow_output = launched("b_late", BackgroundKind::Shell)
        .attach(FakeTool::new("Slow", true, "done").output);
    setup.tools = vec![
        launcher("Keep", launched("b_keep", BackgroundKind::Shell)),
        Arc::new(Stubborn {
            started: started.clone(),
            gate: gate.clone(),
            output: slow_output,
        }),
    ];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, finished).await;
    built.session.send(submit("c2", "second")).await;
    started.notified().await;
    let second = prompts(&built.session.view().await.unwrap().messages)[1].clone();
    // 编辑先进 actor（中止回合、记下要换），之后工具才结束、报告它起的 b_late：这时 b_late 还不在会话的任何名单里
    let (ack, ()) = tokio::join!(
        built
            .session
            .send(envelope("e1", replace(&second, "second again"))),
        async {
            built.session.view().await.unwrap();
            gate.notify_one();
        }
    );
    assert!(
        matches!(
            ack,
            Ack::Accepted {
                result: CommandResult::Rewound { .. }
            }
        ),
        "{ack:?}"
    );
    until(&mut events, finished).await;
    assert_eq!(*agents.stopped.lock().unwrap(), ["b_late"]);
    let view = built.session.view().await.unwrap();
    assert_eq!(work_ids(&view.works), ["b_keep"]);
}

/// 被停的后台子代理：它之后的 killed 通知 ack 了、记了结束，不开回合、不进新分支的对话。
#[tokio::test]
async fn a_stopped_agent_s_notice_does_not_reach_the_new_branch() {
    let (sender, receiver) = mpsc::unbounded_channel();
    let agents = Arc::new(Agents {
        receiver: Mutex::new(Some(receiver)),
        ..Agents::default()
    });
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_spawn", "Spawn", &json!({})),
        text_response(&["one"]),
        text_response(&["edited"]),
    ]));
    let mut setup = Setup::new(model.clone());
    setup.agents = agents.clone();
    let child = BackgroundChange::Launched {
        id: "agent_1".to_owned(),
        kind: BackgroundKind::Agent,
        description: "Review".to_owned(),
        child: Some(SessionId::new("sess_subagent_agent_1")),
    };
    setup.tools = vec![launcher("Spawn", child)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, finished).await;
    let first = prompts(&built.session.view().await.unwrap().messages)[0].clone();
    built
        .session
        .send(envelope("e1", replace(&first, "first again")))
        .await;
    until(&mut events, finished).await;
    assert_eq!(*agents.stopped.lock().unwrap(), ["agent_1"]);
    sender
        .send(AgentSignal::Notice(AgentNotice {
            seq: 7,
            agent: AgentId::new("agent_1"),
            status: BackgroundStatus::Killed,
            text: "<system-reminder>\nagent killed\n</system-reminder>".to_owned(),
        }))
        .unwrap();
    let seen = until(
        &mut events,
        |event| matches!(event, KernelEvent::BackgroundEnded { work } if work == "agent_1"),
    )
    .await;
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, KernelEvent::TurnStarted { .. })),
        "{seen:?}"
    );
    assert_eq!(*agents.acked.lock().unwrap(), [7]);
    let view = built.session.view().await.unwrap();
    assert!(view.works.is_empty());
    let history = serde_json::to_string(&view.messages).unwrap();
    assert!(!history.contains("agent killed"), "{history}");
    assert_eq!(model.requests().len(), 3);
}
