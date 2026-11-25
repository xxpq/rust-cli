//! 会话 actor 记下的 hook 运行（docs/specs/rust-cli/session-lifecycle.md §10.51）：回合里的挂在在跑的回合上，SessionStart 先待挂、
//! 之后第一个输入或定时任务开的回合开始时挂上。只在内存（照 v4：hook 事件不落库），视图带记下的全部。

use rust_cli_kernel_api::hook_runs::{BlockedPrompt, HookInvocation, TurnHooks};
use rust_cli_kernel_api::ids::TurnId;
use rust_cli_kernel_api::session_api::{KernelEvent, TurnOrigin};

use crate::actor::Actor;

/// 记下的与待挂的。
#[derive(Debug, Default)]
pub(crate) struct HookRows {
    recorded: Vec<TurnHooks>,
    /// SessionStart（不在回合里跑，§3.2）：等之后第一个用户看得见的回合。
    pending: Vec<HookInvocation>,
}

impl HookRows {
    /// 视图的 `hooks`：挂上了的（待挂的不在）。
    pub(crate) fn recorded(&self) -> &[TurnHooks] {
        &self.recorded
    }

    /// 挂在这个回合上：`seq` 是这一轮里第几次。
    fn record(
        &mut self,
        turn: TurnId,
        invocation: HookInvocation,
        blocked: Option<BlockedPrompt>,
    ) -> TurnHooks {
        let seq = self
            .recorded
            .iter()
            .filter(|hooks| hooks.turn == turn)
            .count();
        let hooks = TurnHooks {
            turn,
            seq,
            invocation,
            blocked,
        };
        self.recorded.push(hooks.clone());
        hooks
    }
}

impl Actor {
    /// 回合里的一次 hook 事件（`SinkRequest::HooksRan`，回合的提交通道）：挂在在跑的回合上、发 `HooksRan`。`blocked` 是 UserPromptSubmit
    /// 拦下开回合的输入时那条输入的原文（消息 id 是这一轮开回合的消息）。
    pub(crate) fn hooks_ran(&mut self, invocation: HookInvocation, blocked: Option<String>) {
        let Some(running) = &self.running else {
            return;
        };
        let blocked = blocked.and_then(|text| {
            running
                .prompt
                .clone()
                .map(|message| BlockedPrompt { message, text })
        });
        let hooks = self
            .hook_rows
            .record(running.turn.clone(), invocation, blocked);
        self.log.append(KernelEvent::HooksRan { hooks });
    }

    /// SessionStart 跑完（`finish_startup`）：先待挂。
    pub(crate) fn session_hooks_ran(&mut self, invocation: HookInvocation) {
        self.hook_rows.pending.push(invocation);
    }

    /// 回合开始（`TurnStarted` 已发）：输入或定时任务开的回合收下待挂的 SessionStart（照 v4：挂在下一条用户看得见的回合上，压缩、通知、
    /// 同伴消息开的回合不收）。
    pub(crate) fn attach_session_hooks(&mut self, turn: &TurnId, origin: TurnOrigin) {
        if !matches!(origin, TurnOrigin::Input | TurnOrigin::Scheduled) {
            return;
        }
        for invocation in std::mem::take(&mut self.hook_rows.pending) {
            let hooks = self.hook_rows.record(turn.clone(), invocation, None);
            self.log.append(KernelEvent::HooksRan { hooks });
        }
    }
}

#[cfg(test)]
#[path = "hook_rows_tests.rs"]
mod tests;
