//! Agent 工具（docs/specs/rust-cli/agents.md §2）：描述与 schema 在 `prompts/tools/Agent.json`；
//! 前台等子代理结束，后台立即返回“已启动”（缺省为后台）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::classifier::ClassifierInput;
use rust_cli_kernel_api::machine_param::MachineParam;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::subagent_runs::SubagentStart;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation, invalid,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::child::Parent;
use crate::definitions::{AgentDefinition, GENERAL_PURPOSE};
use crate::live::{AgentState, LiveAgent};
use crate::report::{self, Ending};
use crate::routing;
use crate::spawn;
use crate::{Core, SubagentLaunch};
use rust_cli_kernel_api::tool_names;

pub(crate) const NAME: &str = tool_names::AGENT;

#[derive(Debug, Deserialize)]
struct Input {
    description: String,
    prompt: String,
    #[serde(default)]
    subagent_type: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    run_in_background: Option<bool>,
}

/// 只给界面的结局（结果的 `status` 与统计，docs/specs/rust-cli/tui-display.md §2.1）：one-shot agent
/// 的结果没有 `<usage>` 尾注，界面的 `Done (…)` 行靠它。
/// 另带子会话与类型（kernel-api `agents::subagent_link`）：会话 actor 把调用关联到子会话落库，Node 的子代理查询
/// 靠它找到一次性子代理（修复线 F2）。
fn with_display(mut output: ToolOutput, entry: &LiveAgent, mut display: Value) -> ToolOutput {
    display["agentId"] = entry.agent.as_str().into();
    display["childSessionId"] = entry.child.as_str().into();
    display["agentType"] = entry.definition.agent_type.as_str().into();
    output.display = serde_json::json!({ (rust_cli_kernel_api::agents::DISPLAY_KEY): display });
    output
}

fn launched_output(entry: &LiveAgent) -> ToolOutput {
    let output = with_display(
        rust_cli_kernel_api::tool::text(report::launched(entry.agent.as_str())),
        entry,
        serde_json::json!({ "status": "async_launched" }),
    );
    // 后台派生、前台停放转后台：会话起了一项后台工作（K-W1，docs/specs/rust-cli/session-lifecycle.md §10.28）
    crate::views::background_launch(entry).attach(output)
}

fn parse_input(input: &Value) -> Result<Input, String> {
    serde_json::from_value(input.clone()).map_err(|error| error.to_string())
}

pub struct AgentTool {
    pub(crate) core: Arc<Core>,
}

impl AgentTool {
    /// 不用 `Agent.json` 时的描述与 schema：fork 打开时是 fork 写法（修复线 L-A，agents.md §8.1.1）；关着、缺省在前台时是
    /// 缺省前台的写法（agents.md §8.2.4）。
    fn other_text(&self) -> Option<&'static rust_cli_prompts::agents::ToolText> {
        if self.core.fork {
            return rust_cli_prompts::agents::fork_tool();
        }
        match self.core.launch {
            SubagentLaunch::Background => None,
            SubagentLaunch::Foreground => rust_cli_prompts::agents::foreground_tool(),
        }
    }

    async fn definition(
        &self,
        input: &Input,
        context: &ToolUseContext,
    ) -> Result<AgentDefinition, String> {
        let wanted = input.subagent_type.as_deref().unwrap_or(GENERAL_PURPOSE);
        // fork 不在 agent 清单里，只在 fork 打开时认（修复线 L-A，agents.md §8.1.1）。
        if self.core.fork && wanted == crate::fork::FORK {
            return Ok(crate::fork::definition());
        }
        let definitions = self
            .core
            .catalog
            .definitions(&context.workspace, &context.additional_directories)
            .await;
        if let Some(found) = definitions.iter().find(|agent| agent.agent_type == wanted) {
            return Ok(found.clone());
        }
        let available: Vec<&str> = definitions
            .iter()
            .map(|agent| agent.agent_type.as_str())
            .collect();
        Err(format!(
            "Agent type '{wanted}' not found. Available agents: {}",
            available.join(", ")
        ))
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn classifier_input(&self, input: &Value) -> ClassifierInput {
        crate::classifier_input::agent(input)
    }

    fn name(&self) -> &str {
        NAME
    }

    /// 子代理在点名的机器上执行（remote-exec.md §20.5.1）：Agent 自己在主 env 上，只把机器交给子代理。
    fn machine_param(&self) -> MachineParam {
        MachineParam::Delegates
    }

    /// fork 写法没有 `run_in_background`（修复线 L-A，agents.md §8.1.1）；缺省前台的写法换了它的说明（agents.md §8.2.4）。
    fn input_schema(&self) -> Value {
        match self.other_text() {
            Some(text) => text.input_schema.clone(),
            None => rust_cli_prompts::definition(NAME).schema.clone(),
        }
    }

    /// Agent 的 schema 是宽松对象（agents.md §2，修复线 F51）：schema 外的键（例如删掉的 `model`，F51b 删掉的
    /// `isolation`）丢掉、不报错。以前 `validate_input` 拒绝 isolation，现在管线已丢掉这个键，那段检查删掉。
    fn strips_unknown_keys(&self) -> bool {
        true
    }

    fn description(&self, _context: &PromptContext) -> String {
        match self.other_text() {
            Some(text) => text.description.clone(),
            None => rust_cli_prompts::definition(NAME).description.clone(),
        }
    }

    /// 权限交给子代理自己的工具，Agent 本身算只读。
    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(rust_cli_kernel_api::tool::DEFAULT_MAX_RESULT_CHARS)
    }

    async fn validate_input(&self, input: &Value, context: &ToolUseContext) -> Validation {
        let input = match parse_input(input) {
            Ok(input) => input,
            Err(message) => return invalid(message),
        };
        match self.definition(&input, context).await {
            Ok(_) => Validation::Ok,
            Err(message) => invalid(message),
        }
    }

    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        // 除 auto 模式的分类器外，派生子代理总是允许（子代理自己的工具仍走权限）。
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let raw = input.clone();
        let input = parse_input(&input).map_err(|message| ToolError::InvalidInput { message })?;
        let definition = self
            .definition(&input, &context)
            .await
            .map_err(|message| ToolError::InvalidInput { message })?;
        let core = &self.core;
        let Some(parent) = core.runs.get(&context.run) else {
            return Err(ToolError::Failed {
                message: "the parent run is not registered".to_owned(),
            });
        };
        // 子代理里派生时同样按参数（以前总在前台：子会话没有 actor 接收通知）。嵌套的后台子代理的
        // 通知送给派生它的子代理（owner），由 `routing` 投递（agents.md §5.1）。fork 打开时一律在后台（对照实现
        // 同样，修复线 L-A）。模型没给参数时按缺省的启动方式（agents.md §8.2.3）。
        let background = self.core.fork
            || input
                .run_in_background
                .unwrap_or(self.core.launch == SubagentLaunch::Background)
            || definition.background;
        let fork = definition.agent_type == crate::fork::FORK && self.core.fork;
        // fork 继承父会话已提交的对话（agents.md §8.1.2）；会话 actor 是它的 owner。
        let inherited = if fork {
            match context.session_ops.conversation().await {
                Some(history) => Some(crate::fork::prefix(
                    core.ports.ids.as_ref(),
                    history,
                    &context.tool_use,
                    raw,
                    &parent.selection.model,
                )),
                None => {
                    return Err(ToolError::Failed {
                        message: "fork is only available in the main conversation".to_owned(),
                    });
                }
            }
        } else {
            None
        };
        let owner = context.agent.clone();
        let depth = owner
            .as_ref()
            .and_then(|owner| core.live.get(owner))
            .map_or(1, |owner| owner.depth + 1);
        let (agent, child) = spawn::ids(core);
        // 子代理的模型与档位：Agent 的 `effort` 优先于定义的，按子代理的模型可选的档位对档（model-options.md §12.2）。
        let intent = input.effort.clone().or_else(|| definition.effort.clone());
        let (selection, execution) = spawn::selections(
            core.ports.model.as_ref(),
            &parent,
            (fork, definition.model.as_deref()),
            intent.as_deref(),
        )
        .await;
        let entry = LiveAgent {
            agent,
            child,
            parent: context.session.clone(),
            root: core.live.root_of(&context.session),
            owner,
            depth,
            tool_use: Some(context.tool_use.clone()),
            description: input.description.clone(),
            selection,
            execution,
            // 派生那一刻的模式取调用上下文里的（调用之前现读的），不是父运行开始时的；之后跟随父会话（agents.md §4.1.3）。
            mode: spawn::mode(context.mode, definition.permission_mode),
            follow: context.session_ops.live_mode().map(crate::child::Follow),
            definition: Arc::new(definition),
            workspace: context.workspace.clone(),
            // 执行管线已按 Agent 的 `machine` 换好了机器（R3）：远端时记下它，子代理的工具缺省在那里跑。
            machine: context.machine.name().clone(),
            background,
            // 前台的条目也记：停放后转成后台、之后的续跑都按后台运行（agents.md §4.1.1）。
            asks: core
                .background_prompts
                .then(|| context.interactions.lasting())
                .flatten()
                .map(crate::child::Asks),
            state: AgentState::Finished(Ending::Completed),
            shown: crate::live::Shown::default(),
            delivered: Vec::new(),
            fork_prefix: inherited.map(Arc::new),
            // 派生它的运行不许用的工具（session-lifecycle.md §10.15）：取自调用上下文里这次运行的工具，名单只在那里。
            disallowed_tools: context.tools.disallowed().to_vec(),
            additional_directories: context.additional_directories.clone(),
            preferences: parent.preferences,
            prompt: parent.prompt.clone(),
            session_rules: context.session_rules.clone(),
        };
        let first = if fork {
            rust_cli_prompts::agents::fork_directive(&input.prompt)
        } else {
            input.prompt.clone()
        };
        let (entry, opening) =
            spawn::create(core, entry, &parent, &first)
                .await
                .map_err(|error| ToolError::Failed {
                    message: error.to_string(),
                })?;
        // 子会话里后台命令的通知送给这个子代理（agents.md §5.2）。
        crate::shell_notices::watch(core, &entry);
        // 派生的事实交给会话（session-lifecycle.md §10.56）：主线程的会话 actor 记进子代理表，记下之后子代理才跑；子代理里派生的由 ChildSink 丢掉。
        context
            .session_ops
            .subagent_started(SubagentStart {
                tool_use: context.tool_use.clone(),
                agent: entry.agent.clone(),
                child: entry.child.clone(),
                agent_type: entry.definition.agent_type.clone(),
                description: entry.description.clone(),
                background,
            })
            .await;
        if background {
            let output = launched_output(&entry);
            spawn::background(core, entry, opening);
            return Ok(output);
        }
        let parent = Parent {
            session: context.session.clone(),
            asks: context.interactions.clone(),
            ops: context.session_ops.clone(),
            progress,
        };
        let finished = spawn::drive(
            core,
            &entry,
            opening,
            Some(parent),
            Some(context.cancel.clone()),
        )
        .await;
        let settled = routing::settle(core, &entry.agent, &finished).await;
        routing::wake(core, settled.wakes).await;
        if settled.parked {
            // 还有在跑的后台子代理（交互模式）：转成后台、停放，父调用回“已启动”（`async_launched`）。
            return Ok(launched_output(&entry));
        }
        match (finished.ending, finished.error) {
            (Ending::Killed(_), _) => Err(ToolError::Cancelled),
            (_, Some(error)) => Err(ToolError::Failed {
                message: error.to_string(),
            }),
            (_, None) => {
                let trailer = !entry.definition.one_shot();
                let text = report::handback(entry.agent.as_str(), &finished.report, trailer);
                let report = &finished.report;
                Ok(with_display(
                    rust_cli_kernel_api::tool::text(text),
                    &entry,
                    serde_json::json!({
                        "status": "completed",
                        "toolUses": report.tool_uses,
                        "tokens": report.tokens,
                        "durationMs": report.duration_ms,
                    }),
                ))
            }
        }
    }
}
