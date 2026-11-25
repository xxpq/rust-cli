//! 主循环输出端与工具的测试替身。

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{Notify, mpsc};

use crate::ids::InteractionId;
use crate::message::{Message, ToolResultContent};
use crate::orchestration::{
    Absorbed, Answer, InteractionAnswer, InteractionRequest, Interactions, OrchestrationError,
    PermissionAnswer, QueryEvent, RunSink, Steered, ToolExecution, ToolExecutor,
};
use crate::store::StoreError;
use crate::tool::{
    InterruptBehavior, PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};

/// 记录提交的消息与事件；`ask` 一律拒绝（权限询问是不带说明的拒绝，提问不回答，计划审批继续计划）。
#[derive(Default)]
pub struct RecordingSink {
    messages: Mutex<Vec<Message>>,
    events: Mutex<Vec<QueryEvent>>,
    steering: Mutex<Vec<String>>,
    /// `commit_metered` 的计量，按提交顺序，带同批消息的 id（usage.md §2.5）。
    metered: Mutex<Vec<(Option<crate::ids::MessageId>, crate::metering::Metered)>>,
    /// 交来的 hook 运行（`SessionOps::hooks_ran`），按顺序。
    hooks: Mutex<Vec<crate::hook_runs::HookInvocation>>,
    /// 报来的子代理的开始（`SessionOps::subagent_started`），按顺序。
    subagents: Mutex<Vec<crate::subagent_runs::SubagentStart>>,
    /// 这个“会话”此刻的权限模式（`SessionOps::live_mode`）：测试 `set` 它，派生的子代理跟随（agents.md §4.1.3）。
    pub mode: Arc<super::ModeCell>,
}

impl RecordingSink {
    /// 下一次 `take_absorbed` 取走的插话（没有 hook 消息）。
    pub fn steer(&self, text: &str) {
        self.steering.lock_unpoisoned().push(text.to_owned());
    }

    pub fn messages(&self) -> Vec<Message> {
        self.messages.lock_unpoisoned().clone()
    }

    pub fn events(&self) -> Vec<QueryEvent> {
        self.events.lock_unpoisoned().clone()
    }

    pub fn metered(&self) -> Vec<(Option<crate::ids::MessageId>, crate::metering::Metered)> {
        self.metered.lock_unpoisoned().clone()
    }

    pub fn hooks(&self) -> Vec<crate::hook_runs::HookInvocation> {
        self.hooks.lock_unpoisoned().clone()
    }

    pub fn subagents(&self) -> Vec<crate::subagent_runs::SubagentStart> {
        self.subagents.lock_unpoisoned().clone()
    }
}

#[async_trait]
impl Interactions for RecordingSink {
    async fn ask(
        &self,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Ok(InteractionAnswer {
            interaction: InteractionId::new("interaction_test"),
            answer: Answer::nobody(&request, PermissionAnswer::Deny { feedback: None }),
        })
    }

    async fn cancel_ask(&self, _tool_use: &crate::ids::ToolUseId) {}

    /// 询问当场按拒绝回答了：替答的总是后到。
    async fn answer_ask(&self, _tool_use: &crate::ids::ToolUseId, _answer: Answer) -> bool {
        false
    }

    /// 不持有会话：没有落更新的 owner。
    async fn apply_permission_updates(
        &self,
        _updates: Vec<crate::permission_updates::PermissionUpdate>,
    ) -> Result<(), OrchestrationError> {
        Err(OrchestrationError::Unsupported {
            what: "permission updates".to_owned(),
        })
    }

    fn lasting(&self) -> Option<Arc<dyn Interactions>> {
        None
    }
}

/// 不持有会话状态（[`super::NoSession`]）；交来的 hook 运行与子代理的开始记下。
#[async_trait]
impl super::OpsOverride for RecordingSink {
    async fn hooks_ran(&self, invocation: crate::hook_runs::HookInvocation) {
        self.hooks.lock_unpoisoned().push(invocation);
    }

    async fn subagent_started(&self, start: crate::subagent_runs::SubagentStart) {
        self.subagents.lock_unpoisoned().push(start);
    }

    fn live_mode(&self) -> Option<Arc<dyn crate::session_ops::LiveMode>> {
        Some(self.mode.clone())
    }
}

#[async_trait]
impl RunSink for RecordingSink {
    async fn commit_message(&self, message: Message) -> Result<(), StoreError> {
        self.messages.lock_unpoisoned().push(message);
        Ok(())
    }

    async fn commit_input(
        &self,
        message: Message,
        _commands: Vec<crate::ids::CommandId>,
    ) -> Result<(), StoreError> {
        self.commit_message(message).await
    }

    async fn commit_metered(
        &self,
        message: Option<Message>,
        metered: crate::metering::Metered,
    ) -> Result<(), StoreError> {
        self.metered
            .lock_unpoisoned()
            .push((message.as_ref().map(|m| m.id().clone()), metered));
        if let Some(message) = message {
            self.messages.lock_unpoisoned().push(message);
        }
        Ok(())
    }

    async fn commit_messages(&self, messages: Vec<Message>) -> Result<(), StoreError> {
        self.messages.lock_unpoisoned().extend(messages);
        Ok(())
    }

    async fn compact_started(&self, _trigger: crate::message::CompactTrigger) {}

    async fn compact_failed(&self, _trigger: crate::message::CompactTrigger, _message: String) {}

    /// `steer` 记下的插话（没有 hook 消息），没有通知。
    async fn take_absorbed(&self) -> Vec<Absorbed> {
        let steered = std::mem::take(&mut *self.steering.lock_unpoisoned());
        Absorbed::ordered(
            steered.into_iter().map(Steered::plain).collect(),
            Vec::new(),
        )
    }

    /// 没有目标：不提交。
    async fn goal_checked(
        &self,
        _condition: String,
        _verdict: crate::hooks::StopVerdict,
    ) -> Result<Vec<Message>, StoreError> {
        Ok(Vec::new())
    }

    async fn emit(&self, event: QueryEvent) -> bool {
        self.events.lock_unpoisoned().push(event);
        true
    }
}

/// 可控的工具：等 `gate` 放行（或被取消）后返回 `output`；`started` 在开始执行时通知。
pub struct FakeTool {
    pub name: String,
    pub safe: bool,
    pub interrupt: InterruptBehavior,
    pub output: ToolOutput,
    pub gate: Option<Arc<Notify>>,
    pub started: Arc<Notify>,
    /// 怎样用 `machine` 参数（缺省不带，[`FakeTool::taking_machine`]）。
    pub machine_param: crate::machine_param::MachineParam,
}

impl FakeTool {
    pub fn new(name: &str, safe: bool, text: &str) -> Self {
        Self {
            name: name.to_owned(),
            safe,
            interrupt: InterruptBehavior::Block,
            output: ToolOutput {
                content: vec![ToolResultContent::Text {
                    text: text.to_owned(),
                }],
                is_error: false,
                display: Value::Null,
            },
            gate: None,
            started: Arc::new(Notify::new()),
            machine_param: crate::machine_param::MachineParam::Absent,
        }
    }

    /// 带 `machine` 参数（工具名不决定这一点，工作线 F137）。
    pub fn taking_machine(mut self, param: crate::machine_param::MachineParam) -> Self {
        self.machine_param = param;
        self
    }

    pub fn failing(mut self) -> Self {
        self.output.is_error = true;
        self
    }

    pub fn gated(mut self, gate: Arc<Notify>) -> Self {
        self.gate = Some(gate);
        self
    }
}

#[async_trait]
impl Tool for FakeTool {
    fn classifier_input(&self, _input: &serde_json::Value) -> crate::classifier::ClassifierInput {
        crate::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        format!("{} test tool", self.name)
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        self.safe
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        self.safe
    }

    fn machine_param(&self) -> crate::machine_param::MachineParam {
        self.machine_param
    }

    fn interrupt_behavior(&self) -> InterruptBehavior {
        self.interrupt
    }

    async fn call(
        &self,
        _input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        self.started.notify_one();
        if let Some(gate) = &self.gate {
            tokio::select! {
                () = gate.notified() => {}
                () = context.cancel.cancelled() => return Err(ToolError::Cancelled),
            }
        }
        Ok(self.output.clone())
    }
}

/// 直接调用工具的执行管线（不做校验、权限与 hooks）。
pub struct DirectExecutor;

#[async_trait]
impl ToolExecutor for DirectExecutor {
    async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolExecution {
        ToolExecution::output(match tool.call(input, context, progress).await {
            Ok(output) => output,
            Err(error) => ToolOutput {
                content: vec![ToolResultContent::Text {
                    text: format!("<tool_use_error>{error}</tool_use_error>"),
                }],
                is_error: true,
                display: Value::Null,
            },
        })
    }
}

/// 固定的工具列表。
pub struct ListTools(pub Vec<Arc<dyn Tool>>);

impl super::RegistryOverride for ListTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.0.clone()
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.0.iter().find(|tool| tool.name() == name).cloned()
    }
}
