//! 会话 actor 记下的主线程子代理（docs/specs/rust-cli/session-lifecycle.md §10.56）：开始经 `SessionOps::subagent_started`（Agent 工具派生之后、
//! 子代理跑之前）；转后台（停放、续跑）跟着后台工作表收下的开始；前台的结束看这次调用的工具结束，后台的结束看终态通知。只在内存，视图带着全部。

use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::{AgentId, SessionId, ToolUseId};
use rust_cli_kernel_api::session_api::KernelEvent;
use rust_cli_kernel_api::subagent_runs::{
    SubagentEnded, SubagentOutcome, SubagentRun, SubagentStart,
};
use rust_cli_kernel_api::tool::ToolOutput;

use crate::actor::Actor;

/// 子代理表：按开始的先后。结束了的也留着（由视图建的投影与活会话同值）。
#[derive(Debug, Default)]
pub(crate) struct SubagentRows {
    runs: Vec<SubagentRun>,
}

/// 一次调用的结果对前台子代理意味着什么结局：被停的调用是取消（§10.17），出错的是失败。
pub(crate) fn finished_outcome(output: &ToolOutput, cancelled: bool) -> SubagentOutcome {
    if cancelled {
        SubagentOutcome::Cancelled
    } else if output.is_error {
        SubagentOutcome::Failed
    } else {
        SubagentOutcome::Completed
    }
}

/// 终态通知的结局；通知只带终态（agents 的 `Ending::status`），`Running` 没有结局。
fn noticed_outcome(status: BackgroundStatus) -> Option<SubagentOutcome> {
    match status {
        BackgroundStatus::Completed => Some(SubagentOutcome::Completed),
        BackgroundStatus::Failed => Some(SubagentOutcome::Failed),
        BackgroundStatus::Killed => Some(SubagentOutcome::Cancelled),
        BackgroundStatus::Running => None,
    }
}

impl SubagentRows {
    pub(crate) fn runs(&self) -> &[SubagentRun] {
        &self.runs
    }

    fn started(&mut self, start: SubagentStart, at: u64) -> SubagentRun {
        let run = SubagentRun {
            start,
            started_at: at,
            ended: None,
        };
        self.runs.push(run.clone());
        run
    }

    /// 后台工作表收下了这个子会话的一次开始：在前台跑 → 转后台（停放）；已结束 → 再跑（SendMessage 续跑）；已在后台跑 → 不动（None）。
    fn launched(&mut self, child: &SessionId, at: u64) -> Option<SubagentRun> {
        let run = self.runs.iter_mut().find(|run| &run.start.child == child)?;
        match run.ended {
            None if run.start.background => return None,
            None => run.start.background = true,
            Some(_) => {
                run.start.background = true;
                run.started_at = at;
                run.ended = None;
            }
        }
        Some(run.clone())
    }

    /// 这次调用派生的、还在前台跑的那一项结束了。
    fn foreground_ended(
        &mut self,
        tool_use: &ToolUseId,
        outcome: SubagentOutcome,
        at: u64,
    ) -> Option<SubagentRun> {
        let run = self.runs.iter_mut().find(|run| {
            &run.start.tool_use == tool_use && !run.start.background && run.ended.is_none()
        })?;
        run.ended = Some(SubagentEnded { outcome, at });
        Some(run.clone())
    }

    /// 这个子代理的终态通知到了：在跑的那一项结束。
    fn noticed(
        &mut self,
        agent: &AgentId,
        outcome: SubagentOutcome,
        at: u64,
    ) -> Option<SubagentRun> {
        let run = self
            .runs
            .iter_mut()
            .find(|run| &run.start.agent == agent && run.ended.is_none())?;
        run.ended = Some(SubagentEnded { outcome, at });
        Some(run.clone())
    }
}

impl Actor {
    fn subagent_recorded(&self, run: Option<SubagentRun>) {
        if let Some(run) = run {
            self.log.append(KernelEvent::SubagentRecorded { run });
        }
    }

    /// Agent 工具派生了一个子代理（`SinkRequest::SubagentStarted`，回合的提交通道）：记进表、发 `SubagentRecorded`。
    pub(crate) fn subagent_started(&mut self, start: SubagentStart) {
        let run = self.subagent_rows.started(start, self.deps.clock.now_ms());
        self.subagent_recorded(Some(run));
    }

    /// 后台工作表收下了一个子代理的开始（主线程的工具结束，§10.28）：停放的转后台，续跑的再跑。
    pub(crate) fn subagent_launched(&mut self, child: &SessionId) {
        let run = self.subagent_rows.launched(child, self.deps.clock.now_ms());
        self.subagent_recorded(run);
    }

    /// 主线程的一个调用结束（处理完它起的后台工作之后）：它派生的子代理还在前台跑，就按这次的结果结束。
    pub(crate) fn subagent_call_finished(
        &mut self,
        tool_use: &ToolUseId,
        outcome: SubagentOutcome,
    ) {
        let run = self
            .subagent_rows
            .foreground_ended(tool_use, outcome, self.deps.clock.now_ms());
        self.subagent_recorded(run);
    }

    /// 子代理的终态通知进了会话（关闭中的不收；在围栏判断之前，§10.55：被截掉的调用派生的子代理也要结束）。
    pub(crate) fn subagent_noticed(&mut self, agent: &AgentId, status: BackgroundStatus) {
        let Some(outcome) = noticed_outcome(status) else {
            return;
        };
        let run = self
            .subagent_rows
            .noticed(agent, outcome, self.deps.clock.now_ms());
        self.subagent_recorded(run);
    }
}

#[cfg(test)]
#[path = "subagent_rows_tests.rs"]
mod tests;
