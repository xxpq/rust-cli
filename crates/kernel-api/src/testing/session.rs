//! 会话操作的测试替身（工作线 F134，审查 K6）：不持有会话状态的 [`NoSession`]，与“全委托、只覆盖关心的方法”的
//! [`OpsOverride`]（实现它的类型就是 [`SessionOps`]）。生产实现直接实现 [`SessionOps`]，漏写一个方法是编译错误。

use std::sync::Arc;

use async_trait::async_trait;

use crate::env::EnvPath;
use crate::hook_runs::HookInvocation;
use crate::ids::{SessionId, ToolUseId};
use crate::machine::CommandEvents;
use crate::message::Message;
use crate::monitor::MonitorArmed;
use crate::orchestration::OrchestrationError;
use crate::permission::auto_mode::{AutoVerdict, DenialLimit};
use crate::session_ops::{LiveMode, RunView, SessionOps};
use crate::subagent_runs::SubagentStart;

fn unsupported(what: &str) -> OrchestrationError {
    OrchestrationError::Unsupported {
        what: what.to_owned(),
    }
}

/// 不持有会话状态：没有视图与对话，owner 是会话自己，命令一律 `Unsupported`。
pub struct NoSession;

#[async_trait]
impl SessionOps for NoSession {
    async fn view(&self) -> Option<RunView> {
        None
    }

    async fn owner_session(&self, session: &SessionId) -> SessionId {
        session.clone()
    }

    async fn conversation(&self) -> Option<Vec<Message>> {
        None
    }

    async fn set_plan(&self, _enabled: bool, _by: &ToolUseId) -> Result<(), OrchestrationError> {
        Err(unsupported("changing plan mode"))
    }

    async fn set_workspace(&self, _root: EnvPath) -> Result<(), OrchestrationError> {
        Err(unsupported("changing the working directory"))
    }

    async fn arm_monitor(
        &self,
        _monitor: MonitorArmed,
        _events: CommandEvents,
    ) -> Result<(), OrchestrationError> {
        Err(unsupported("monitors"))
    }

    /// 没有会话，hook 行没有人读：丢掉。
    async fn hooks_ran(&self, _invocation: HookInvocation) {}

    /// 没有会话，子代理行没有人读：丢掉。
    async fn subagent_started(&self, _start: SubagentStart) {}

    /// 没有会话，不计数。
    async fn note_auto_verdict(&self, _verdict: AutoVerdict) -> Option<DenialLimit> {
        None
    }

    /// 没有会话，没有模式可跟。
    fn live_mode(&self) -> Option<Arc<dyn LiveMode>> {
        None
    }
}

/// 测试里能改的会话模式（[`LiveMode`]，agents.md §4.1.3）：[`ModeCell::set`] 之后 `mode` 读到新值；没设过时没有回答（同父会话
/// 已经不在）。
#[derive(Default)]
pub struct ModeCell(std::sync::Mutex<Option<crate::permission::PermissionMode>>);

impl ModeCell {
    pub fn set(&self, mode: crate::permission::PermissionMode) {
        *rust_cli_base::sync::LockUnpoisoned::lock_unpoisoned(&self.0) = Some(mode);
    }
}

#[async_trait]
impl LiveMode for ModeCell {
    async fn mode(&self) -> Option<crate::permission::PermissionMode> {
        *rust_cli_base::sync::LockUnpoisoned::lock_unpoisoned(&self.0)
    }
}

/// 会话操作的测试代理：每个方法缺省转给 [`OpsOverride::ops`]（缺省是 [`NoSession`]），替身只覆盖关心的方法。
#[async_trait]
pub trait OpsOverride: Send + Sync {
    fn ops(&self) -> &dyn SessionOps {
        &NoSession
    }

    async fn view(&self) -> Option<RunView> {
        self.ops().view().await
    }

    async fn owner_session(&self, session: &SessionId) -> SessionId {
        self.ops().owner_session(session).await
    }

    async fn conversation(&self) -> Option<Vec<Message>> {
        self.ops().conversation().await
    }

    async fn set_plan(&self, enabled: bool, by: &ToolUseId) -> Result<(), OrchestrationError> {
        self.ops().set_plan(enabled, by).await
    }

    async fn set_workspace(&self, root: EnvPath) -> Result<(), OrchestrationError> {
        self.ops().set_workspace(root).await
    }

    async fn arm_monitor(
        &self,
        monitor: MonitorArmed,
        events: CommandEvents,
    ) -> Result<(), OrchestrationError> {
        self.ops().arm_monitor(monitor, events).await
    }

    async fn hooks_ran(&self, invocation: HookInvocation) {
        self.ops().hooks_ran(invocation).await;
    }

    async fn subagent_started(&self, start: SubagentStart) {
        self.ops().subagent_started(start).await;
    }

    async fn note_auto_verdict(&self, verdict: AutoVerdict) -> Option<DenialLimit> {
        self.ops().note_auto_verdict(verdict).await
    }

    fn live_mode(&self) -> Option<Arc<dyn LiveMode>> {
        self.ops().live_mode()
    }
}

#[async_trait]
impl<T: OpsOverride> SessionOps for T {
    async fn view(&self) -> Option<RunView> {
        OpsOverride::view(self).await
    }

    async fn owner_session(&self, session: &SessionId) -> SessionId {
        OpsOverride::owner_session(self, session).await
    }

    async fn conversation(&self) -> Option<Vec<Message>> {
        OpsOverride::conversation(self).await
    }

    async fn set_plan(&self, enabled: bool, by: &ToolUseId) -> Result<(), OrchestrationError> {
        OpsOverride::set_plan(self, enabled, by).await
    }

    async fn set_workspace(&self, root: EnvPath) -> Result<(), OrchestrationError> {
        OpsOverride::set_workspace(self, root).await
    }

    async fn arm_monitor(
        &self,
        monitor: MonitorArmed,
        events: CommandEvents,
    ) -> Result<(), OrchestrationError> {
        OpsOverride::arm_monitor(self, monitor, events).await
    }

    async fn hooks_ran(&self, invocation: HookInvocation) {
        OpsOverride::hooks_ran(self, invocation).await;
    }

    async fn subagent_started(&self, start: SubagentStart) {
        OpsOverride::subagent_started(self, start).await;
    }

    async fn note_auto_verdict(&self, verdict: AutoVerdict) -> Option<DenialLimit> {
        OpsOverride::note_auto_verdict(self, verdict).await
    }

    fn live_mode(&self) -> Option<Arc<dyn LiveMode>> {
        OpsOverride::live_mode(self)
    }
}
