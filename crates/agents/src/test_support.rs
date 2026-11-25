//! 测试替身：假的子循环工厂（记录它看到的工具、system 段与请求）、调用工具的父运行。

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::TaskScope;
use rust_cli_base::models::GLM_5_3;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::agents::{SubagentQueries, TaskListStore};
use rust_cli_kernel_api::controller_services::ControllerServices;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, Generation, OperationId, RunId, SessionId};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::message::{AssistantMessage, ContentBlock, Message, StopReason, Usage};
use rust_cli_kernel_api::model::{QuerySource, SystemBlock};
use rust_cli_kernel_api::orchestration::{
    ContextBuilder, ContextRequest, QueryError, QueryRequest, QueryRunner, QueryStop, RunSink,
    ToolRegistry,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::run_tools::RunTools;
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::{
    FakeTool, FixedClock, FixedContext, ListTools, MemoryStore, RecordingSink, SequentialIds,
    UnusedEnvironment, controller_services, machine, selection, unsupported_machine,
};
use rust_cli_kernel_api::tool::{Tool, ToolOutput, ToolUseContext, Validation};
use serde_json::Value;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{Agents, AgentsConfig, AgentsPorts, MemoryTaskLists, SubagentLaunch};

/// 测试内核的执行环境 generation（`AgentsPorts::generation`）。
pub(crate) const GENERATION: &str = "gen_agents_test";

/// 子循环看到的东西。
#[derive(Clone)]
pub(crate) struct Seen {
    pub(crate) tools: Vec<String>,
    pub(crate) system: Vec<SystemBlock>,
    pub(crate) request: QueryRequest,
}

/// 子循环在哪里等（后台场景）；中止时提前返回。
#[derive(Clone)]
pub(crate) enum Gate {
    /// 所有运行共用一个。
    Shared(Arc<Notify>),
    /// 每个子会话一个（嵌套场景：分别放行），见 [`Gates::release`]。
    PerSession(Gates),
}

/// 按子会话的放行信号；先放行后等待也不会丢（`Notify` 存一个许可）。
#[derive(Clone, Default)]
pub(crate) struct Gates(Arc<Mutex<std::collections::HashMap<SessionId, Arc<Notify>>>>);

impl Gates {
    fn of(&self, session: &SessionId) -> Arc<Notify> {
        let mut gates = self.0.lock_unpoisoned();
        gates.entry(session.clone()).or_default().clone()
    }

    pub(crate) fn release(&self, session: &str) {
        self.of(&SessionId::new(session)).notify_one();
    }
}

#[derive(Default)]
pub(crate) struct FakeQueries {
    pub(crate) seen: Arc<Mutex<Vec<Seen>>>,
    pub(crate) gate: Option<Gate>,
    /// 放行后、结束前在“工具边界”取走的通知：(子会话, 通知全文)。
    pub(crate) absorbed: Arc<Mutex<Vec<(SessionId, String)>>>,
    /// 为真时放行后直接结束、没有工具边界（插话与通知都留着）：模拟消息在最后一个工具边界之后才到。
    pub(crate) no_boundary: Arc<std::sync::atomic::AtomicBool>,
    /// 有值时中止之后等它放行才返回：模拟停了、还没退出的运行（agents.md §6）。
    pub(crate) linger: Linger,
    /// 放行之后子循环取的视图里的模式：(子会话, 模式)（agents.md §4.1.3：父会话中途换的模式，子代理下一次取视图就用上）。
    pub(crate) viewed: Viewed,
}

/// [`FakeQueries::viewed`]。
pub(crate) type Viewed = Arc<Mutex<Vec<(SessionId, Option<PermissionMode>)>>>;

/// 中止之后扣住子循环的放行信号（[`FakeQueries::linger`]）；`None` 时中止立即返回。
pub(crate) type Linger = Arc<Mutex<Option<Arc<Notify>>>>;

struct FakeRunner {
    tools: Arc<dyn ToolRegistry>,
    context: Arc<dyn ContextBuilder>,
    seen: Arc<Mutex<Vec<Seen>>>,
    gate: Option<Gate>,
    absorbed: Arc<Mutex<Vec<(SessionId, String)>>>,
    no_boundary: Arc<std::sync::atomic::AtomicBool>,
    linger: Linger,
    viewed: Viewed,
}

impl SubagentQueries for FakeQueries {
    fn runner(
        &self,
        tools: Arc<dyn ToolRegistry>,
        context: Arc<dyn ContextBuilder>,
    ) -> Arc<dyn QueryRunner> {
        Arc::new(FakeRunner {
            tools,
            context,
            seen: self.seen.clone(),
            gate: self.gate.clone(),
            absorbed: self.absorbed.clone(),
            no_boundary: self.no_boundary.clone(),
            linger: self.linger.clone(),
            viewed: self.viewed.clone(),
        })
    }
}

pub(crate) fn report_message(text: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: rust_cli_kernel_api::ids::MessageId::new("msg_report"),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        model: GLM_5_3.to_owned(),
        stop_reason: Some(StopReason::EndTurn),
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 3,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }),
    })
}

#[async_trait]
impl QueryRunner for FakeRunner {
    async fn run(
        &self,
        request: QueryRequest,
        sink: Arc<dyn RunSink>,
        abort: Abort,
    ) -> Result<QueryStop, QueryError> {
        let context = ContextRequest {
            agent: request.agent.clone(),
            ..ContextRequest::new(
                request.session.clone(),
                request.workspace.clone(),
                request.source.clone(),
                request.mode,
                request.selection.clone(),
            )
        };
        let system = self
            .context
            .build(context, &request.messages)
            .await
            .unwrap()
            .system;
        let tools = self
            .tools
            .tools()
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect();
        let session = request.session.clone();
        self.seen.lock_unpoisoned().push(Seen {
            tools,
            system,
            request,
        });
        let gate = match &self.gate {
            Some(Gate::Shared(gate)) => Some(gate.clone()),
            Some(Gate::PerSession(gates)) => Some(gates.of(&session)),
            None => None,
        };
        if let Some(gate) = gate {
            tokio::select! {
                () = gate.notified() => {}
                () = abort.aborted() => {
                    let linger = self.linger.lock_unpoisoned().clone();
                    if let Some(linger) = linger {
                        linger.notified().await;
                    }
                    return Ok(QueryStop::Aborted);
                }
            }
        }
        // 执行管线在每个工具调用之前取一次视图（query `executor.rs`）：记下放行之后这一次的模式。
        let mode = sink.view().await.map(|view| view.mode());
        self.viewed.lock_unpoisoned().push((session.clone(), mode));
        // 一个工具边界：取走发给这次运行的插话与通知（query.md §5.1），记下通知。工作线 F134：`RunSink` 只有
        // `take_absorbed`（以前这里只取 `take_notices`，插话留在邮箱）；要插话留着的测试关掉这个边界。
        let absorbed = if self.no_boundary.load(std::sync::atomic::Ordering::SeqCst) {
            Vec::new()
        } else {
            sink.take_absorbed().await
        };
        for item in absorbed {
            if let rust_cli_kernel_api::orchestration::Absorbed::Notice(notice) = item {
                self.absorbed
                    .lock_unpoisoned()
                    .push((session.clone(), notice.text));
            }
        }
        sink.commit_message(report_message("Hello from the subagent."))
            .await?;
        Ok(QueryStop::Completed)
    }
}

/// 父运行：在登记过的运行里调用一个工具，记下结果。校验不过时结果同执行管线
/// （`<tool_use_error>…</tool_use_error>`，is_error）。
pub(crate) struct ParentRunner {
    pub(crate) registry: Arc<dyn ToolRegistry>,
    /// 工具调用看到的机器与本机服务。
    pub(crate) machine: MachineEnv,
    pub(crate) controller: ControllerServices,
    pub(crate) tool: String,
    pub(crate) input: Value,
    pub(crate) output: Arc<Mutex<Option<ToolOutput>>>,
    /// 父运行的会话操作（主线程的会话 actor 的替身）：记下 Agent 报来的子代理的开始。
    pub(crate) ops: Arc<RecordingSink>,
    /// 这次调用的 id；None 时是 testing 的缺省 `toolu_1`。
    pub(crate) tool_use: Option<rust_cli_kernel_api::ids::ToolUseId>,
    /// 调用上下文里的模式；None 时是 [`tool_context`] 的 dontAsk。
    pub(crate) mode: Option<PermissionMode>,
}

pub(crate) fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/work"),
    }
}

/// 一个在跑的后台子代理条目（Explore，模型是 testing 的缺省选择），由测试插进 `Live`。
pub(crate) fn running(agent: &str, root: &str) -> crate::live::LiveAgent {
    crate::live::LiveAgent {
        machine: Default::default(),
        agent: rust_cli_kernel_api::ids::AgentId::new(agent),
        child: SessionId::new(format!("sess_subagent_{agent}")),
        parent: SessionId::new(root),
        root: SessionId::new(root),
        owner: None,
        depth: 1,
        tool_use: None,
        description: format!("work of {agent}"),
        definition: Arc::new(
            crate::definitions::builtins()
                .into_iter()
                .find(|definition| definition.agent_type == "Explore")
                .unwrap(),
        ),
        workspace: workspace(),
        selection: selection(),
        execution: None,
        mode: PermissionMode::Default,
        follow: None,
        background: true,
        asks: None,
        state: crate::live::AgentState::running(Vec::new()),
        shown: crate::live::Shown::default(),
        delivered: Vec::new(),
        fork_prefix: None,
        disallowed_tools: Vec::new(),
        additional_directories: Vec::new(),
        preferences: Default::default(),
        prompt: Default::default(),
        session_rules: Vec::new(),
    }
}

/// 父运行的设置（[`Harness::call_with`]）：不许用的工具与附加工作目录。
#[derive(Default)]
pub(crate) struct ParentRun<'a> {
    pub(crate) disallowed: &'a [&'a str],
    pub(crate) directories: &'a [&'a str],
    pub(crate) preferences: rust_cli_kernel_api::session_preferences::SessionPreferences,
    /// 这次调用的 id（缺省 `toolu_1`）：要分清几次调用的用例（SendMessage 续跑之后的通知）给它。
    pub(crate) tool_use: Option<&'a str>,
    /// 调用上下文里的模式（缺省同父运行开始时的 dontAsk）：调用之前现读的，可能已经不是运行开始时的（agents.md §4.1.3）。
    pub(crate) mode: Option<PermissionMode>,
}

pub(crate) fn tool_context(session: &SessionId, run: &RunId) -> ToolUseContext {
    let env = Arc::new(UnusedEnvironment);
    ToolUseContext {
        session: session.clone(),
        run: run.clone(),
        workspace: workspace(),
        mode: PermissionMode::DontAsk,
        machine: machine(env.clone()),
        controller: controller_services(env),
        interactions: Arc::new(RecordingSink::default()),
        session_ops: Arc::new(RecordingSink::default()),
        operation: OperationContext::new(
            EnvironmentId::new("local"),
            Generation::new("g1"),
            OperationId::new("op_1"),
            Origin::Agent {
                session: session.clone(),
                run: run.clone(),
            },
        ),
        ..rust_cli_kernel_api::testing::tool_use_context()
    }
}

#[async_trait]
impl QueryRunner for ParentRunner {
    async fn run(
        &self,
        request: QueryRequest,
        _sink: Arc<dyn RunSink>,
        _abort: Abort,
    ) -> Result<QueryStop, QueryError> {
        let tool = self.registry.find(&self.tool).unwrap();
        let (progress, _updates) = mpsc::channel(16);
        let mut context = tool_context(&request.session, &request.run);
        context.agent = request.agent.clone();
        // 同主循环的执行器：调用上下文带这次运行的工具（注册表去掉这次运行的禁用名单，session-lifecycle.md §10.15）。
        context.tools = Arc::new(RunTools::new(
            self.registry.clone(),
            &request.disallowed_tools,
        ));
        context.machine = self.machine.clone();
        context.controller = self.controller.clone();
        context.additional_directories = request.additional_directories.clone();
        context.session_ops = self.ops.clone();
        if let Some(tool_use) = &self.tool_use {
            context.tool_use = tool_use.clone();
        }
        if let Some(mode) = self.mode {
            context.mode = mode;
        }
        let output = match tool.validate_input(&self.input, &context).await {
            Validation::Invalid { message, .. } => ToolOutput {
                content: vec![ToolResultContent::Text {
                    text: format!("<tool_use_error>{message}</tool_use_error>"),
                }],
                is_error: true,
                display: Value::Null,
            },
            Validation::Ok => tool
                .call(self.input.clone(), context, progress)
                .await
                .unwrap(),
        };
        *self.output.lock_unpoisoned() = Some(output);
        Ok(QueryStop::Completed)
    }
}

pub(crate) struct Harness {
    pub(crate) agents: Agents,
    pub(crate) store: Arc<MemoryStore>,
    pub(crate) tasks: Arc<MemoryTaskLists>,
    pub(crate) seen: Arc<Mutex<Vec<Seen>>>,
    pub(crate) absorbed: Arc<Mutex<Vec<(SessionId, String)>>>,
    /// 假的子循环没有工具边界（[`FakeQueries::no_boundary`]）。
    pub(crate) no_boundary: Arc<std::sync::atomic::AtomicBool>,
    /// 中止之后扣住子循环（[`FakeQueries::linger`]）。
    pub(crate) linger: Linger,
    /// 放行之后子循环取的视图里的模式（[`FakeQueries::viewed`]）。
    pub(crate) viewed: Viewed,
    /// 父运行里工具调用看到的机器与本机服务（缺省什么都不支持）。
    pub(crate) machine: MachineEnv,
    pub(crate) controller: ControllerServices,
    /// 只读快照问过的压缩器（F112）：每次问的模型与档位。
    pub(crate) compactor: Arc<ContextFake>,
    /// 父运行的会话操作（[`ParentRunner::ops`]）。
    pub(crate) ops: Arc<RecordingSink>,
    pub(crate) _scope: TaskScope,
}

/// 只读快照用的压缩器替身（修复线 F112，tui-panels.md §1.7）：上下文用量是“历史条数 × 1000 / 10000”，提示是
/// `Used{历史条数}`；记下每次问的（模型，档位），好核对用的是子代理的选择。
#[derive(Default)]
pub(crate) struct ContextFake {
    pub(crate) asked: Mutex<Vec<(String, Option<String>)>>,
}

impl ContextFake {
    fn ask(&self, selection: &rust_cli_kernel_api::model::ModelSelection) {
        self.asked
            .lock_unpoisoned()
            .push((selection.model.clone(), selection.reasoning.clone()));
    }
}

#[async_trait]
impl rust_cli_kernel_api::compaction::Compactor for ContextFake {
    fn should_auto_compact(
        &self,
        _history: &[Message],
        _selection: &rust_cli_kernel_api::model::ModelSelection,
    ) -> bool {
        false
    }

    fn context_usage(
        &self,
        history: &[Message],
        selection: &rust_cli_kernel_api::model::ModelSelection,
    ) -> Option<rust_cli_kernel_api::compaction::ContextOccupancy> {
        self.ask(selection);
        Some(rust_cli_kernel_api::compaction::ContextOccupancy {
            used_tokens: history.len() as u64 * 1_000,
            max_tokens: 10_000,
            compact_at: None,
            window: None,
        })
    }

    fn context_notice(
        &self,
        history: &[Message],
        _selection: &rust_cli_kernel_api::model::ModelSelection,
    ) -> Option<rust_cli_kernel_api::compaction::ContextNotice> {
        Some(rust_cli_kernel_api::compaction::ContextNotice::Used {
            percent: history.len() as u64,
        })
    }

    async fn compact_metered(
        &self,
        _request: rust_cli_kernel_api::compaction::CompactRequest,
        _cancel: CancellationToken,
        _meter: &dyn rust_cli_kernel_api::compaction::CompactMeter,
    ) -> Result<
        rust_cli_kernel_api::compaction::CompactResult,
        rust_cli_kernel_api::orchestration::OrchestrationError,
    > {
        Err(
            rust_cli_kernel_api::orchestration::OrchestrationError::Unsupported {
                what: "compact".to_owned(),
            },
        )
    }
}

pub(crate) fn harness(gate: Option<Arc<Notify>>) -> Harness {
    built(
        gate.map(Gate::Shared),
        true,
        machine(Arc::new(UnusedEnvironment)),
        no_peers(),
    )
}

fn no_peers() -> Arc<dyn rust_cli_kernel_api::peers::PeerMessaging> {
    Arc::new(rust_cli_kernel_api::peers::NoPeers)
}

/// 同伴会话换成 `peers`（cross-session.md §1.3、§1.4）。
pub(crate) fn with_peers(peers: Arc<dyn rust_cli_kernel_api::peers::PeerMessaging>) -> Harness {
    built(None, true, machine(Arc::new(UnusedEnvironment)), peers)
}

/// 每个子会话分别放行；`keep_alive` 是交互模式（停放）还是 `-p`。
pub(crate) fn held(gates: &Gates, keep_alive: bool) -> Harness {
    held_with_env(gates, keep_alive, unsupported_machine())
}

/// 同 [`held`]，agents 自己用的机器换成 `machine`（子会话里后台命令的通知，agents.md §5.2）。
pub(crate) fn held_with_env(gates: &Gates, keep_alive: bool, machine: MachineEnv) -> Harness {
    built(
        Some(Gate::PerSession(gates.clone())),
        keep_alive,
        machine,
        no_peers(),
    )
}

/// 基础注册表按规则藏起 `hidden` 里的名字（整个工具的 deny 规则，tools-files.md §1.1.1）。
pub(crate) fn hiding(hidden: &'static [&'static str]) -> Harness {
    built_with(
        None,
        true,
        machine(Arc::new(UnusedEnvironment)),
        no_peers(),
        hidden,
        Arc::new(FixedClock(1_000)),
    )
}

/// 藏起一些名字的基础注册表：自己的工具里去掉它们，`hides` 告诉外层。
struct Hiding {
    inner: ListTools,
    hidden: &'static [&'static str],
}

impl rust_cli_kernel_api::testing::RegistryOverride for Hiding {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools = ToolRegistry::tools(&self.inner);
        tools.retain(|tool| !self.hidden.contains(&tool.name()));
        tools
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        ToolRegistry::find(&self.inner, name).filter(|_| !self.hidden.contains(&name))
    }

    fn hides(&self, name: &str) -> bool {
        self.hidden.contains(&name)
    }
}

fn built(
    gate: Option<Gate>,
    keep_alive: bool,
    machine: MachineEnv,
    peers: Arc<dyn rust_cli_kernel_api::peers::PeerMessaging>,
) -> Harness {
    built_with(
        gate,
        keep_alive,
        machine,
        peers,
        &[],
        Arc::new(FixedClock(1_000)),
    )
}

/// 同 [`held_with_env`]，另给时钟（子代理里 monitor 的批与到期按时钟算，agents.md §5.3）。
pub(crate) fn held_with_clock(
    gates: &Gates,
    keep_alive: bool,
    machine: MachineEnv,
    clock: Arc<dyn rust_cli_kernel_api::clock::Clock>,
) -> Harness {
    built_with(
        Some(Gate::PerSession(gates.clone())),
        keep_alive,
        machine,
        no_peers(),
        &[],
        clock,
    )
}

/// 同 [`harness`]，另给执行环境的注册表（R3：远端机器上的 TaskStop、子代理的缺省机器）。
pub(crate) fn with_machines(
    machines: Arc<dyn rust_cli_kernel_api::env_registry::EnvironmentRegistry>,
) -> Harness {
    let local = machines.primary_machine();
    built_full(
        None,
        (false, SubagentLaunch::default()),
        local,
        no_peers(),
        &[],
        Arc::new(FixedClock(1_000)),
        (Some(machines), None),
    )
}

fn built_with(
    gate: Option<Gate>,
    keep_alive: bool,
    machine_env: MachineEnv,
    peers: Arc<dyn rust_cli_kernel_api::peers::PeerMessaging>,
    hidden: &'static [&'static str],
    clock: Arc<dyn rust_cli_kernel_api::clock::Clock>,
) -> Harness {
    built_full(
        gate,
        (keep_alive, SubagentLaunch::default()),
        machine_env,
        peers,
        hidden,
        clock,
        (None, None),
    )
}

/// 子代理缺省的启动方式换成 `launch`（agents.md §8.2）。
pub(crate) fn launching(launch: SubagentLaunch) -> Harness {
    built_full(
        None,
        (true, launch),
        machine(Arc::new(UnusedEnvironment)),
        no_peers(),
        &[],
        Arc::new(FixedClock(1_000)),
        (None, None),
    )
}

/// 模型端口答的档位是 `levels`（子代理的 `effort` 按它对档，model-options.md §12.2）。
pub(crate) fn with_levels(levels: &[&str]) -> Harness {
    let levels = levels.iter().map(|level| (*level).to_owned()).collect();
    built_full(
        None,
        (true, SubagentLaunch::default()),
        machine(Arc::new(UnusedEnvironment)),
        no_peers(),
        &[],
        Arc::new(FixedClock(1_000)),
        (None, Some(levels)),
    )
}

fn built_full(
    gate: Option<Gate>,
    (keep_alive, launch): (bool, SubagentLaunch),
    machine_env: MachineEnv,
    peers: Arc<dyn rust_cli_kernel_api::peers::PeerMessaging>,
    hidden: &'static [&'static str],
    clock: Arc<dyn rust_cli_kernel_api::clock::Clock>,
    (machines, levels): (
        Option<Arc<dyn rust_cli_kernel_api::env_registry::EnvironmentRegistry>>,
        Option<Vec<String>>,
    ),
) -> Harness {
    let scope = TaskScope::root("agents-test", |_| {});
    let store = Arc::new(MemoryStore::default());
    let tasks = Arc::new(MemoryTaskLists::default());
    let queries = FakeQueries {
        gate,
        ..FakeQueries::default()
    };
    let seen = queries.seen.clone();
    let absorbed = queries.absorbed.clone();
    let no_boundary = queries.no_boundary.clone();
    let linger = queries.linger.clone();
    let viewed = queries.viewed.clone();
    let base: Vec<Arc<dyn Tool>> = ["Bash", "Read", "Edit", "Workflow"]
        .into_iter()
        .map(|name| Arc::new(FakeTool::new(name, true, "")) as Arc<dyn Tool>)
        .collect();
    let compactor = Arc::new(ContextFake::default());
    // 操作上下文与别的 ID 同一个来源（ID 的顺序不变，工作线 F134）。
    let ids: Arc<dyn rust_cli_kernel_api::clock::Ids> = Arc::new(SequentialIds::default());
    let ports = AgentsPorts {
        store: store.clone(),
        ids: ids.clone(),
        clock,
        compactor: compactor.clone(),
        machines: machines.unwrap_or_else(|| {
            Arc::new(rust_cli_kernel_api::env_registry::PrimaryRegistry::new(
                machine_env.clone(),
                rust_cli_kernel_api::testing::controller_services(Arc::new(UnusedEnvironment)),
            ))
        }),
        catalog: Arc::new(crate::AgentCatalog::new(
            Arc::new(UnusedEnvironment),
            Arc::new(UnusedEnvironment),
        )),
        tasks: tasks.clone() as Arc<dyn TaskListStore>,
        queries: Arc::new(queries),
        context: Arc::new(FixedContext(Vec::new())),
        spawner: scope.spawner(),
        peers,
        operations: rust_cli_kernel_api::env::Operations::new(
            rust_cli_kernel_api::ids::Generation::new(GENERATION),
            ids,
        ),
        model: Arc::new(
            rust_cli_kernel_api::testing::ScriptedModel::new(Vec::new()).levels(levels),
        ),
    };
    let agents = Agents::new(
        ports,
        Arc::new(Hiding {
            inner: ListTools(base),
            hidden,
        }),
        AgentsConfig {
            task_tools: crate::TaskTools::On,
            keep_alive,
            // 交互模式（停放）时后台子代理也能询问（agents.md §4.1.1）；测试的交互替身不给会话级入口。
            background_prompts: keep_alive,
            // fork 子代理由 TUI 的集成测试与 fork_tests.rs 覆盖（agents.md §8.1）。
            fork: false,
            launch,
        },
    );
    Harness {
        agents,
        store,
        tasks,
        seen,
        absorbed,
        no_boundary,
        linger,
        viewed,
        machine: machine(Arc::new(UnusedEnvironment)),
        controller: controller_services(Arc::new(UnusedEnvironment)),
        compactor,
        ops: Arc::default(),
        _scope: scope,
    }
}

impl Harness {
    /// 父运行里工具调用看到的机器换成 `machine`（controller 的服务不变，都返回 `Unsupported`）。
    pub(crate) fn set_machine(&mut self, machine: MachineEnv) {
        self.machine = machine;
    }

    /// 在会话 `session` 的一次运行里调用 `tool`。
    pub(crate) async fn call(&self, session: &str, tool: &str, input: Value) -> ToolOutput {
        self.call_with(session, tool, input, ParentRun::default())
            .await
    }

    /// 同 [`Self::call`]，这次运行另有不许用的工具（session-lifecycle.md §10.15）与附加工作目录（session-dirs.md §3.3）。
    pub(crate) async fn call_with(
        &self,
        session: &str,
        tool: &str,
        input: Value,
        ParentRun {
            disallowed,
            directories,
            preferences,
            tool_use,
            mode,
        }: ParentRun<'_>,
    ) -> ToolOutput {
        let output = Arc::new(Mutex::new(None));
        let parent = ParentRunner {
            registry: self.agents.registry(),
            machine: self.machine.clone(),
            controller: self.controller.clone(),
            tool: tool.to_owned(),
            input,
            output: output.clone(),
            ops: self.ops.clone(),
            tool_use: tool_use.map(rust_cli_kernel_api::ids::ToolUseId::new),
            mode,
        };
        let runner = self.agents.track(Arc::new(parent));
        let session = SessionId::new(session);
        self.store
            .create(
                rust_cli_kernel_api::store::SessionMeta {
                    id: session.clone(),
                    kind: rust_cli_kernel_api::store::SessionKind::Interactive,
                    parent: None,
                    workspace: workspace(),
                    title: String::new(),
                    trace: rust_cli_kernel_api::ids::TraceId::new(session.as_str()),
                    created_at_ms: 0,
                },
                rust_cli_kernel_api::testing::no_facts(),
            )
            .await
            .unwrap();
        // 子会话里的调用带子代理 ID（与主循环相同）：`sess_subagent_<id>` 里的 `<id>`。
        let agent = session
            .as_str()
            .strip_prefix("sess_subagent_")
            .map(rust_cli_kernel_api::ids::AgentId::new);
        let request = QueryRequest {
            agent,
            disallowed_tools: disallowed.iter().map(|name| (*name).to_owned()).collect(),
            additional_directories: directories.iter().map(|dir| EnvPath::new(*dir)).collect(),
            preferences,
            ..QueryRequest::new(
                session,
                RunId::new(format!("run_parent_{tool}")),
                workspace(),
                Vec::new(),
                selection(),
                PermissionMode::DontAsk,
                QuerySource::MainThread,
            )
        };
        runner
            .run(request, Arc::new(RecordingSink::default()), Abort::new())
            .await
            .unwrap();
        output.lock_unpoisoned().take().unwrap()
    }

    /// 后台子代理在自己的任务里跑：让出执行权，直到放行之后的视图至少取了 `count` 次。
    pub(crate) async fn viewed(&self, count: usize) -> Vec<(SessionId, Option<PermissionMode>)> {
        for _ in 0..10_000 {
            let viewed = self.viewed.lock_unpoisoned().clone();
            if viewed.len() >= count {
                return viewed;
            }
            tokio::task::yield_now().await;
        }
        panic!("the subagent loop did not take the view {count} times");
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock_unpoisoned().clone()
    }

    /// 后台子代理在自己的任务里跑：让出执行权，直到子循环至少开始了 `count` 次。
    pub(crate) async fn started(&self, count: usize) -> Vec<Seen> {
        for _ in 0..10_000 {
            let seen = self.seen();
            if seen.len() >= count {
                return seen;
            }
            tokio::task::yield_now().await;
        }
        panic!("the subagent loop did not start {count} times");
    }
}

pub(crate) fn text(output: &ToolOutput) -> String {
    match output.content.as_slice() {
        [rust_cli_kernel_api::message::ToolResultContent::Text { text }] => text.clone(),
        other => panic!("unexpected content {other:?}"),
    }
}

/// 单测里不关心时刻的 [`crate::live::Live`]：时钟固定在 0（以前是 live.rs 里只给测试用的 `ZeroClock`）。
impl Default for crate::live::Live {
    fn default() -> Self {
        Self::new(Arc::new(FixedClock(0)))
    }
}
