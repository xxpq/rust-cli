//! 主循环（docs/specs/rust-cli/README.md §3）。
//!
//! 包括 transition 状态机、流式工具执行器（tool_use 块
//! 结束就开工、按输入判定并发、结果顺序、Bash 出错不取消其余调用、按中止原因回填）、恢复分支、
//! 中途输入、发给模型前的消息整理。
//!
//! 只依赖 kernel-api 的端口与编排 trait；实现由 runtime 注入。消息、操作意图都经 [`RunSink`]
//! 提交，提交成功后才继续（提交屏障）。
//!
//! 已实现：文本回路、工具执行器与中断语义。恢复分支、自动压缩、Stop hooks、中途输入属于
//! 工作线 L1 的后续步骤。

mod assembler;
mod compaction;
mod defaults;
mod deferred;
#[cfg(test)]
mod disallowed_tests;
mod execution;
mod executor;
mod goal;
mod limits;
mod machines;
mod materialize;
#[cfg(test)]
mod materialize_tests;
#[cfg(test)]
mod mcp_support_tests;
mod mcp_wait;
mod normalize;
mod output_budget;
mod pairing;
mod prefix;
mod recovery;
mod request;
mod respond;
mod run;
mod step;
mod stop_hooks;
mod texts;

pub use deferred::step_tool_search;
pub use limits::{RequestLimits, STOP_HOOK_BLOCK_CAP, StopHookCap, ThinkingTokens};
pub use prefix::RequestPrefixes;

/// `/context` 的消息估算（docs/specs/rust-cli/context-usage.md §3 i）：与主循环相同的规范化（对话中间的
/// system 消息按生效配置保留或降级，`configured` 答不出时按模型卡片，model.md §11），不带每轮 effort。
///
/// 修改（工作线 F93，用户决定 2026-10-24）：`/context` 只用本地估算、不发 count_tokens，F89 为计数请求加的 ToolSearch
/// 参数（历史里的 `tool_reference` 先经主请求的降级函数，端点才认）随之删掉。
pub fn api_messages<'a>(
    messages: impl IntoIterator<Item = &'a rust_cli_kernel_api::message::Message>,
    model: &str,
    configured: Option<bool>,
) -> Vec<rust_cli_kernel_api::model::ApiMessage> {
    let options = normalize::NormalizeOptions {
        system_turns: rust_cli_prompts::models::mid_conversation_system(model, configured),
        turn_effort: None,
        // 缓存断点与主循环相同（摘要请求才前移一条，compact.md）。
        skip_cache_write: false,
    };
    normalize::normalize(messages, &options)
}

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::env::Operations;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::ids::{AgentId, RunId, SessionId};
use rust_cli_kernel_api::model::ModelPort;
use rust_cli_kernel_api::orchestration::{
    Compactor, ContextBuilder, HookRunner, QueryError, QueryRequest, QueryRunner, QueryStop,
    RunSink, RunView, ToolExecutor, ToolRegistry,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::run_tools::RunTools;
use rust_cli_kernel_api::store::ArtifactStore;

/// 主循环用到的能力，由 runtime 注入。
#[derive(Clone)]
pub struct QueryParts {
    pub model: Arc<dyn ModelPort>,
    pub context: Arc<dyn ContextBuilder>,
    pub tools: Arc<dyn ToolRegistry>,
    pub executor: Arc<dyn ToolExecutor>,
    pub compactor: Arc<dyn Compactor>,
    pub hooks: Arc<dyn HookRunner>,
    pub ids: Arc<dyn Ids>,
    /// 模型请求与工具调用的计时（docs/specs/rust-cli/usage.md §1）：计时只在这里读时钟。
    pub clock: Arc<dyn rust_cli_kernel_api::clock::Clock>,
    /// 执行环境的注册表（R3，remote-exec.md §20.5、§20.6）：工具调用缺省在它的主 env 上执行（按调用的 `machine` 换，
    /// §15.8.1），controller 的服务（WebFetch）也从它取；连上的远端机器决定工具定义里的 `machine` 参数；子代理的缺省机器
    /// 从这里取。工作线 F131：以前另存一份主 env 与 controller 的服务，与注册表给的是同一份。
    pub machines: Arc<dyn EnvironmentRegistry>,
    pub artifacts: Arc<dyn ArtifactStore>,
    /// 操作上下文的工厂：执行环境的当前实例（本地是进程 uuid）与 `ids`（F134：以前另带一个 `generation`）。
    pub operations: Operations,
    /// 非交互会话（`-p`）：主线程的断流续写只在这时进行。
    pub non_interactive: bool,
    /// 入口按环境变量给的输出上限与 thinking 开关（query.md §1.5，修复线 F42）。
    pub limits: RequestLimits,
    /// 主线程请求的已定前缀（docs/specs/rust-cli/request-prefix.md §2）：组装根打开时给出（产品的入口），每一步只规范化切点
    /// 之后的消息；None 时每一步整段（内核的单测、子循环）。
    pub prefixes: Option<Arc<RequestPrefixes>>,
}

/// 一次运行内不变的上下文。
pub(crate) struct RunScope {
    pub(crate) session: SessionId,
    pub(crate) run: RunId,
    pub(crate) workspace: WorkspaceRef,
    pub(crate) agent: Option<AgentId>,
    pub(crate) mode: PermissionMode,
    /// 这次运行的缺省机器（主 env 或子代理派生时的机器；R3，remote-exec.md §20.6）：取自请求，一次运行里不变。
    pub(crate) default_machine: rust_cli_kernel_api::ids::MachineName,
    pub(crate) abort: Abort,
    pub(crate) sink: Arc<dyn RunSink>,
    pub(crate) parts: QueryParts,
    /// 本次运行里等满过预算的 MCP 服务器（直接调用连接中服务器的工具，ext.md §3.8）。
    pub(crate) exhausted: mcp_wait::Exhausted,
    /// 这次运行所属的顶层会话（`QueryRequest::root`，工作线 F145）：`parts.tools` 是它的视图。
    pub(crate) root: SessionId,
    /// 这次运行看到的工具（kernel-api `RunTools`，tools-more.md §3.5）：`parts.tools` 就是它（同一个对象，按注册表用）；
    /// 执行器把它放进每个调用的上下文，ToolSearch 与 Agent 工具从那里取。
    pub(crate) tools: Arc<RunTools>,
    /// 输出端不持有会话时的附加工作目录（`QueryRequest::additional_directories`，session-dirs.md §3.3）。
    pub(crate) additional_directories: Vec<rust_cli_kernel_api::env::EnvPath>,
    /// 这次运行的会话偏好（`QueryRequest::preferences`，app-prefs.md §4.2）：上下文请求与压缩请求带着它。
    pub(crate) preferences: rust_cli_kernel_api::session_preferences::SessionPreferences,
    /// 输出端不持有会话时（压缩的摘要请求）每一步用的钉住的输入（`QueryRequest::prompt`，context.md §15.3）。
    pub(crate) prompt: Arc<rust_cli_kernel_api::session_prompt::SessionPrompt>,
}

impl RunScope {
    /// 一次运行的上下文：会话、运行、工作区、子代理、模式与缺省机器取自请求。
    pub(crate) fn new(
        request: &QueryRequest,
        sink: Arc<dyn RunSink>,
        parts: QueryParts,
        abort: Abort,
    ) -> Self {
        // 工具注册表在运行开始时取一次顶层会话的视图（会话级的 MCP 连接只在视图里，工作线 F145），去掉这次运行不许用的
        // 工具（session-lifecycle.md §10.15）：之后的清单、快照、查找、MCP 等待与调用上下文里的工具都是这一个。会话偏好关掉的工具
        // （ToolSearch，app-prefs.md §4.3）并进这份名单，子代理经名单继承。
        let root = request.root().clone();
        let tools = Arc::new(RunTools::new(
            parts.tools.clone().view(&root),
            &request.preferences.disallowed(&request.disallowed_tools),
        ));
        let parts = QueryParts {
            tools: tools.clone(),
            ..parts
        };
        Self {
            session: request.session.clone(),
            run: request.run.clone(),
            workspace: request.workspace.clone(),
            agent: request.agent.clone(),
            mode: request.mode,
            default_machine: request.default_machine.clone(),
            abort,
            sink,
            parts,
            exhausted: Default::default(),
            root,
            tools,
            additional_directories: request.additional_directories.clone(),
            preferences: request.preferences,
            prompt: request.prompt.clone(),
        }
    }

    /// 带给请求、工具上下文、hook 与压缩的顶层会话（kernel-api 里 `root` 字段的约定：None 是会话自己）：子代理的运行才与 `session` 不同
    /// （platform.md §5.3），经宿主取账号凭据的来源按它找会话。
    pub(crate) fn nested_root(&self) -> Option<SessionId> {
        (self.root != self.session).then(|| self.root.clone())
    }

    /// 会话现在的状态：输出端的一次读取（同一时刻的值，工作线 F134）。会话 actor 是唯一 owner，工具改过的模式与工作目录
    /// 在同一回合里立即生效（docs/specs/rust-cli/tools-more.md §2.3、§11.4）；输出端不持有会话的状态时用请求里的模式与
    /// 工作区、缺省的计划模式标志，没有主模型与目标（context.md §13.4）。
    pub(crate) async fn view(&self) -> RunView {
        self.sink.view().await.unwrap_or_else(|| RunView {
            additional_directories: self.additional_directories.clone(),
            prompt: self.prompt.clone(),
            ..RunView::new(self.mode, self.workspace.clone())
        })
    }
}

pub struct QueryLoop {
    parts: QueryParts,
}

impl QueryLoop {
    pub fn new(parts: QueryParts) -> Self {
        Self { parts }
    }

    pub fn parts(&self) -> &QueryParts {
        &self.parts
    }
}

#[async_trait]
impl QueryRunner for QueryLoop {
    async fn run(
        &self,
        request: QueryRequest,
        sink: Arc<dyn RunSink>,
        abort: Abort,
    ) -> Result<QueryStop, QueryError> {
        let scope = Arc::new(RunScope::new(&request, sink, self.parts.clone(), abort));
        run::run(scope, request).await
    }
}
