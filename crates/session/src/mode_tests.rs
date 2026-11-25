//! 工具经 `set_plan` 打开 plan：先落库再通知，同一回合里后续的工具调用看到新的判决模式
//! （docs/specs/rust-cli/tools-more.md §2.3）。

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, Generation, SessionId, TraceId};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::permission::{ExecutionState, PermissionMode};
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, Delivery, KernelEvent, SessionHandle, StartSource,
};
use rust_cli_kernel_api::store::{Fact, SessionKind, SessionMeta, SessionStore};
use rust_cli_kernel_api::testing::{
    DirectExecutor, FixedClock, FixedContext, ListTools, MemoryArtifacts, MemoryStore,
    NeverCompact, NoHooks, ScriptedModel, SequentialIds, UnusedEnvironment, selection,
    text_response, tool_use_response,
};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use rust_cli_query::{QueryLoop, QueryParts};
use serde_json::{Value, json};

use crate::{Opened, Session, SessionDeps, SessionState};

/// 输入里有 `plan` 时请求打开 plan；结果总是调用时看到的模式。每次调用记下会话的 `live_mode` 入口（agents.md §4.1.3）。
#[derive(Default)]
struct ModeTool {
    kept: std::sync::Mutex<Option<Arc<dyn rust_cli_kernel_api::session_ops::LiveMode>>>,
}

#[async_trait::async_trait]
impl Tool for ModeTool {
    fn classifier_input(
        &self,
        _input: &serde_json::Value,
    ) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        "Mode"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: tokio::sync::mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let seen = format!("{:?}", context.mode);
        *rust_cli_base::sync::LockUnpoisoned::lock_unpoisoned(&self.kept) =
            context.session_ops.live_mode();
        if input.get("plan").is_some() {
            context
                .session_ops
                .set_plan(true, &context.tool_use)
                .await
                .map_err(|error| ToolError::Failed {
                    message: error.to_string(),
                })?;
        }
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text { text: seen }],
            is_error: false,
            display: Value::Null,
        })
    }
}

fn meta() -> SessionMeta {
    SessionMeta {
        id: SessionId::new("sess_1"),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        title: String::new(),
        trace: TraceId::new("trace_1"),
        created_at_ms: 0,
    }
}

/// 会话 actor（工具清单里只有 `ModeTool`）。
fn spawn(
    scope: &TaskScope,
    store: &Arc<MemoryStore>,
    model: &Arc<ScriptedModel>,
    mode: PermissionMode,
) -> Session {
    spawn_with(scope, store, model, mode, Arc::new(ModeTool::default()))
}

fn spawn_with(
    scope: &TaskScope,
    store: &Arc<MemoryStore>,
    model: &Arc<ScriptedModel>,
    mode: PermissionMode,
    tool: Arc<ModeTool>,
) -> Session {
    let ids = Arc::new(SequentialIds::default());
    let env = Arc::new(UnusedEnvironment);
    let runner = Arc::new(QueryLoop::new(QueryParts {
        model: model.clone(),
        context: Arc::new(FixedContext(Vec::new())),
        tools: Arc::new(ListTools(vec![tool])),
        executor: Arc::new(DirectExecutor),
        compactor: Arc::new(NeverCompact),
        hooks: Arc::new(NoHooks),
        ids: ids.clone(),
        clock: Arc::new(rust_cli_kernel_api::testing::FixedClock(0)),
        machines: rust_cli_kernel_api::testing::environments(env),
        artifacts: store.clone(),
        operations: rust_cli_kernel_api::env::Operations::new(Generation::new("g1"), ids.clone()),
        non_interactive: false,
        limits: rust_cli_query::RequestLimits::default(),
        prefixes: None,
    }));
    let env = Arc::new(UnusedEnvironment);
    Session::spawn(
        SessionDeps {
            journal: Arc::new(rust_cli_kernel_api::input_journal::NoJournal),
            policy: rust_cli_kernel_api::session_policy::SessionPolicy::default(),
            agents: Arc::new(rust_cli_kernel_api::agents::NoAgentSignals),
            cron: Arc::new(rust_cli_kernel_api::cron::NoCron),
            store: store.clone(),
            ids: ids.clone(),
            runner,
            compactor: Arc::new(NeverCompact),
            machines: rust_cli_kernel_api::testing::environments(env),
            tools: Arc::new(rust_cli_kernel_api::testing::NoTools),
            max_turns: None,
            hooks: Arc::new(NoHooks),
            commands: Arc::new(rust_cli_kernel_api::user_command::NoUserCommands),
            inputs: Arc::new(rust_cli_kernel_api::user_input::NoUserInputs),
            artifacts: Arc::new(MemoryArtifacts::default()),
            clock: Arc::new(FixedClock(0)),
            operations: rust_cli_kernel_api::env::Operations::new(Generation::new("g1"), ids),
            spawner: scope.spawner(),
            closing: scope.spawner(),
            titles: None,
            side_model: Arc::new(rust_cli_kernel_api::testing::ScriptedModel::new(Vec::new())),
            released: Arc::new(crate::NothingToRelease),
            directory_rules: Arc::new(rust_cli_kernel_api::orchestration::NoDirectoryRules),
            peers: crate::PeerDeps::off(),
            detached: None,
            hold_queue: false,
            forks: Arc::default(),
            frontend_prompt: None,
        },
        SessionState {
            pending_inputs: Vec::new(),
            meta: meta(),
            messages: Vec::new(),
            earlier: Vec::new(),
            selection: selection(),
            mode: mode.into(),
            checkpoints: Vec::new(),
            reverted: Vec::new(),
            start: StartSource::Startup,
            turn: None,
            last_turn_failed: false,
            turns: Vec::new(),
            inputs: Vec::new(),
            sources: Vec::new(),
            output_heads: Vec::new(),
            tool_times: Vec::new(),
            plans: Vec::new(),
            draft: None,
            additional_directories: Vec::new(),
            preferences: Default::default(),
            session_rules: Vec::new(),
            prompt: crate::PromptPin::created(Default::default()),
        },
        Opened::Created,
    )
}

#[tokio::test]
async fn a_tool_changes_the_mode_for_the_rest_of_the_turn() {
    let scope = TaskScope::root("test", |_| {});
    let store = Arc::new(MemoryStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Mode", &json!({"plan": true})),
        tool_use_response("toolu_2", "Mode", &json!({})),
        text_response(&["done"]),
    ]));
    let session = spawn(&scope, &store, &model, PermissionMode::Default);
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "plan it".to_owned(),
                extras: Default::default(),
                delivery: Delivery::Queue,
                target: None,
            },
        })
        .await;
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap()
            .event;
        let finished = matches!(event, KernelEvent::TurnFinished { .. });
        seen.push(event);
        if finished {
            break;
        }
    }
    // 修复线 F20：只打开 plan；权限模式不变，事件是 PlanChanged。
    assert!(
        seen.iter()
            .any(|event| matches!(event, KernelEvent::PlanChanged { enabled: true, .. }))
    );
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, KernelEvent::ModeChanged { .. }))
    );
    let view = session.view().await.unwrap();
    assert_eq!((view.mode, view.plan), (PermissionMode::Default, true));
    // 第一个调用看到的是旧模式，第二个调用（同一回合）看到 plan。
    let requests = model.requests();
    let third = format!("{:?}", requests[2].messages);
    assert!(third.contains("Default"), "{third}");
    assert!(third.contains("Plan"), "{third}");
    // 模式先落库（没有命令 ID：来自工具）。
    let facts = store.facts(&SessionId::new("sess_1"));
    assert!(facts.contains(&Fact::ExecutionChanged {
        state: ExecutionState::from_parts(PermissionMode::Default, true),
    }));
    // 工具切换不写本机项目的持久模式（Node 只在 `setMode` 写，config.md §2.2）。
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact, Fact::ProjectModeSet { .. }))
    );
}

/// 用户切换模式：与 `ExecutionChanged` 同一批写项目持久模式，值是切换后的权限模式（Node
/// `saveProjectPermissionMode({mode: runtime.getMode()})`，config.md §2.2）。修复线 F20：`plan` 只打开 plan、写当前的
/// 权限模式（每次都写，同 Node）；改权限模式不动 plan；`SetPlan` 不写持久模式。
#[tokio::test]
async fn user_mode_switches_write_the_project_mode() {
    let scope = TaskScope::root("test", |_| {});
    let store = Arc::new(MemoryStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let session = spawn(&scope, &store, &model, PermissionMode::Default);
    let switches = [
        PermissionMode::AcceptEdits,
        PermissionMode::Plan,
        PermissionMode::Plan,
        PermissionMode::DontAsk,
    ];
    for (index, mode) in switches.into_iter().enumerate() {
        let ack = session
            .send(CommandEnvelope {
                id: CommandId::new(format!("mode_{index}")),
                command: Command::SetMode { mode },
            })
            .await;
        assert!(
            matches!(ack, rust_cli_kernel_api::session_api::Ack::Accepted { .. }),
            "{ack:?}"
        );
    }
    let ack = session
        .send(CommandEnvelope {
            id: CommandId::new("plan_off"),
            command: Command::SetPlan { enabled: false },
        })
        .await;
    assert!(matches!(
        ack,
        rust_cli_kernel_api::session_api::Ack::Accepted { .. }
    ));
    let facts: Vec<Fact> = store
        .facts(&SessionId::new("sess_1"))
        .into_iter()
        .filter(|fact| {
            matches!(
                fact,
                Fact::ExecutionChanged { .. } | Fact::ProjectModeSet { .. }
            )
        })
        .collect();
    let changed = |mode, plan| Fact::ExecutionChanged {
        state: ExecutionState::from_parts(mode, plan),
    };
    let saved = |mode| Fact::ProjectModeSet { mode };
    assert_eq!(
        facts,
        [
            changed(PermissionMode::AcceptEdits, false),
            saved(PermissionMode::AcceptEdits),
            changed(PermissionMode::AcceptEdits, true),
            saved(PermissionMode::AcceptEdits),
            changed(PermissionMode::AcceptEdits, true),
            saved(PermissionMode::AcceptEdits),
            changed(PermissionMode::DontAsk, true),
            saved(PermissionMode::DontAsk),
            changed(PermissionMode::DontAsk, false),
        ]
    );
    let view = session.view().await.unwrap();
    assert_eq!((view.mode, view.plan), (PermissionMode::DontAsk, false));
}

/// 会话级的模式入口（agents.md §4.1.3）：工具调用时拿到的 `live_mode` 在回合结束之后照样能问，`SetMode` 之后读到新模式；
/// 子代理派生时拿走它，跟随父会话此刻的模式。修 bug 之前没有这个入口，子代理的模式在派生时定下。
#[tokio::test]
async fn the_live_mode_follows_the_session_after_the_turn_ends() {
    let scope = TaskScope::root("test", |_| {});
    let store = Arc::new(MemoryStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Mode", &json!({})),
        text_response(&["done"]),
    ]));
    let tool = Arc::new(ModeTool::default());
    let session = spawn_with(
        &scope,
        &store,
        &model,
        PermissionMode::AcceptEdits,
        tool.clone(),
    );
    let mut events = session.subscribe(Some(0));
    let submit = |id: &str, command: Command| CommandEnvelope {
        id: CommandId::new(id),
        command,
    };
    session
        .send(submit(
            "c1",
            Command::Submit {
                text: "go".to_owned(),
                extras: Default::default(),
                delivery: Delivery::Queue,
                target: None,
            },
        ))
        .await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap()
            .event;
        if matches!(event, KernelEvent::TurnFinished { .. }) {
            break;
        }
    }
    let live = rust_cli_base::sync::LockUnpoisoned::lock_unpoisoned(&tool.kept)
        .clone()
        .expect("the tool call got a live mode");
    assert_eq!(live.mode().await, Some(PermissionMode::AcceptEdits));
    session
        .send(submit(
            "c2",
            Command::SetMode {
                mode: PermissionMode::BypassPermissions,
            },
        ))
        .await;
    assert_eq!(live.mode().await, Some(PermissionMode::BypassPermissions));
}
