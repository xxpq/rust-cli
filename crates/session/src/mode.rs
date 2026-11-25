//! 执行状态的修改（docs/specs/rust-cli/tools-more.md §2.3、shell-permissions.md §4.6）。执行状态（权限模式 ×
//! plan 开关）只有会话 actor 一个 owner：前端的 `SetMode` / `SetPlan` 命令与工具的 `SessionOps::set_plan`
//! （EnterPlanMode、ExitPlanMode）走同一个入口，先提交 `ExecutionChanged` 事实，成功后才改内存里的状态并通知前端。
//! 主循环经 `SessionOps::view` 读判决用的模式（plan 开时是 plan），所以同一回合里后续的工具调用立即按新状态判决。
//! 工具不改权限模式（工作线 F134 删掉了没有调用方的 `Interactions::set_mode` 与这里的 `change_mode`）。
//!
//! 修复线 F20（用户决定）：plan 是独立开关。以前 plan 是一个模式，进入前的模式不在这里（TUI 记 `before_plan`，
//! ExitPlanMode 按审批的回答换模式）；现在权限模式一直在 actor 里，开关 plan 不动它，退出 plan 后就是进入前的值。
//!
//! 计划模式的两个会话标志也在这里（docs/specs/rust-cli/context.md §13.4）：改状态成功后按判决用的（旧, 新）
//! 模式更新，提交 `plan_mode_reentry` / `plan_mode_exit` 后清掉对应的标志。
//!
//! 一条输入的执行配置（session-lifecycle.md §10.25）也从这里写进会话：开回合时 [`Actor::adopt`]，与命令同一个入口。

use rust_cli_kernel_api::ids::{CommandId, ToolUseId};
use rust_cli_kernel_api::input_extras::TurnConfig;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::{ExecutionState, PermissionMode};
use rust_cli_kernel_api::session_api::{KernelEvent, PlanSwitch};
use rust_cli_kernel_api::store::{Fact, StoreError};

use crate::actor::Actor;

/// 谁改的执行状态：决定提交带不带命令 id、写不写项目的持久模式、`PlanChanged` 带不带调用、放行的同伴消息能不能现在开回合。
enum Cause {
    /// 前端的命令（`SetMode`、`SetPlan`、「完全访问」）；`save` 时同一批写项目的持久模式。
    Command { id: CommandId, save: bool },
    /// 工具（EnterPlanMode、ExitPlanMode）。
    Tool(ToolUseId),
    /// 开回合时套用这条输入的配置（§10.25）：回合就要开，放行的同伴消息排在它之后。
    Input,
}

impl Actor {
    /// 判决、工具、上下文与 hook 看的模式：plan 开时是 plan，否则是权限模式。
    pub(crate) fn mode(&self) -> PermissionMode {
        self.execution.effective()
    }

    /// 工具开关 plan（EnterPlanMode、ExitPlanMode）：权限模式不变，不写项目的持久模式。`by` 是切它的调用（§10.25：前端据它知道
    /// 这次切换来自工具）。
    pub(crate) async fn change_plan(
        &mut self,
        enabled: bool,
        by: ToolUseId,
    ) -> Result<(), StoreError> {
        self.apply(Cause::Tool(by), self.execution.with_plan(enabled))
            .await
    }

    /// 换会话的模型（前端的 `SetModel`，开回合时套用输入的配置）：先落库，再改内存、发事件。
    pub(crate) async fn change_model(
        &mut self,
        command: Option<CommandId>,
        selection: ModelSelection,
    ) -> Result<(), StoreError> {
        let fact = Fact::ModelSelected {
            selection: selection.clone(),
        };
        self.commit(command, vec![fact]).await?;
        self.selection = selection.clone();
        self.log.append(KernelEvent::ModelChanged { selection });
        // F27：有效窗口随模型变，上下文提示与 footer 的 ctx 按新模型重算（按当前模型算）；修前要等下一条回复。
        self.context_changed();
        Ok(())
    }

    /// 开回合时把这条输入的执行配置写成会话的配置（§10.25，照 v4 开跑时套用）：只换给了、并且不同的项；不写项目的持久模式。
    /// 提交不带命令 id：它不是这条输入的效果记录（效果记录是它的消息，§10.3），否则套用之后、消息落库之前退出，重发会被当成重放。
    pub(crate) async fn adopt(&mut self, config: &TurnConfig) -> Result<(), StoreError> {
        if let Some(selection) = config.new_selection(&self.selection) {
            self.change_model(None, selection.clone()).await?;
        }
        let next = config.execution_over(self.execution);
        if next != self.execution {
            // 装箱：`apply` 放行同伴消息时可以开回合（start_next → start_turn → adopt），async 的递归要一层间接。`Cause::Input` 不开。
            Box::pin(self.apply(Cause::Input, next)).await?;
        }
        Ok(())
    }

    /// 用户切换权限模式（前端的 `SetMode`，shift+tab 与 `/mode`）：同一批写本机项目的持久模式
    /// （docs/specs/rust-cli/config.md §2.2，Node `setMode` 写 `runtime.getMode()`，即切换后的权限模式；`plan`
    /// 只打开 plan，写的仍是当前的权限模式）。plan 开关不变（Node 的 `setMode` 会关掉 plan，见 spec 的待确认）。
    pub(crate) async fn user_mode(
        &mut self,
        command: CommandId,
        mode: PermissionMode,
    ) -> Result<(), StoreError> {
        let cause = Cause::Command {
            id: command,
            save: true,
        };
        self.apply(cause, self.execution.with_mode(mode)).await
    }

    /// 权限框里选「放行并切模式」（`ResolveSwitchingMode`，docs/specs/rust-cli/session-lifecycle.md §10.22、§10.59）：只改这个会话的权限模式，
    /// 不写项目的持久模式。
    ///
    /// 修 bug（主会话审查 GAPS-RUST）：以前走 [`Self::user_mode`]，一次询问里的选择写进项目，这个项目之后新建的会话都是完全访问。
    /// 照 v4：输入框里切模式写项目，权限框里的授权只改当前任务；所以同 `user_plan` 走不写项目的同一入口。
    pub(crate) async fn session_mode(
        &mut self,
        command: CommandId,
        mode: PermissionMode,
    ) -> Result<(), StoreError> {
        let cause = Cause::Command {
            id: command,
            save: false,
        };
        self.apply(cause, self.execution.with_mode(mode)).await
    }

    /// 用户开关 plan（前端的 `SetPlan`，`/plan`）：同 App 的 plan 开关（Node `setExecutionState({planEnabled})`），
    /// 不写项目的持久模式。
    pub(crate) async fn user_plan(
        &mut self,
        command: CommandId,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let cause = Cause::Command {
            id: command,
            save: false,
        };
        self.apply(cause, self.execution.with_plan(enabled)).await
    }

    async fn apply(&mut self, cause: Cause, next: ExecutionState) -> Result<(), StoreError> {
        let mut facts = vec![Fact::ExecutionChanged { state: next }];
        let (command, by) = match cause {
            Cause::Command { id, save } => {
                if save {
                    facts.push(Fact::ProjectModeSet { mode: next.mode });
                }
                (Some(id), None)
            }
            Cause::Tool(by) => (None, Some(by)),
            Cause::Input => (None, None),
        };
        let start = command.is_some() || by.is_some();
        self.commit(command, facts).await?;
        self.execution_changed(next, start, by).await;
        Ok(())
    }

    /// 执行状态已提交（`ExecutionChanged` 在刚落库的那一批里）：改内存、更新计划模式的标志、发 `ModeChanged` / `PlanChanged`。
    /// `start`：放行的同伴消息能不能现在开回合；`by` 是切 plan 的工具调用。一组权限更新的 `SetMode` 也经这里收尾
    /// （permission_updates.rs，docs/specs/rust-cli/permission-updates.md §4）。
    pub(crate) async fn execution_changed(
        &mut self,
        next: ExecutionState,
        start: bool,
        by: Option<ToolUseId>,
    ) {
        let previous = self.execution;
        self.plan_flags = self
            .plan_flags
            .after_transition(previous.effective(), next.effective());
        self.execution = next;
        if previous.mode != next.mode {
            self.log
                .append(KernelEvent::ModeChanged { mode: next.mode });
            // 模式类别可能变了：扣住的同伴消息重新判断（cross-session.md §1.6）。
            self.peers_mode_changed(start).await;
        }
        if previous.plan != next.plan {
            // 工具切的 plan 记下是哪个调用（§10.25）：只在这里写，与 plan 开关同一个 owner。
            if let Some(by) = &by {
                self.plan_switch = Some(PlanSwitch {
                    tool_use: by.clone(),
                    enabled: next.plan,
                });
            }
            self.log.append(KernelEvent::PlanChanged {
                enabled: next.plan,
                by,
            });
        }
    }

    /// 已提交的消息若是 context 产生的 reentry / exit，清掉对应的标志。
    pub(crate) fn plan_attachment_committed(&mut self, message: &Message) {
        if let Message::Attachment(attachment) = message {
            self.plan_flags = self.plan_flags.after_commit(&attachment.source);
        }
    }
}

#[cfg(test)]
#[path = "mode_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "plan_flags_tests.rs"]
mod plan_flags_tests;
