//! 会话内定时任务的触发（docs/specs/rust-cli/tools-more.md §10.4）。
//!
//! 任务表的 owner 是 cron 能力（[`CronSignals`]）；排队输入的 owner 是 actor。actor 在同一个 `select!`
//! 里收唤醒，只在空闲时取走到期任务（任务只在 REPL 空闲时触发，以优先级 `later` 排队）。
//! 忙时收到的唤醒直接忽略：回合结束时 `start_next` 会再取。

use rust_cli_kernel_api::cron::CronFire;

use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::admission::Admission;
use rust_cli_kernel_api::ids::TurnId;
use rust_cli_kernel_api::session_api::{Ack, Command, CommandResult};

use crate::actor::{Actor, accepted, rejected};

/// 定时任务开启的回合里，user 消息的来源。
pub(crate) const CRON_SOURCE: &str = "scheduled-task";

/// 唤醒流（流结束后不再等它）。
pub(crate) type CronStream = crate::watched::Watched<()>;

impl Actor {
    /// 停止：能不能中止由准入给出（§10.4、§10.5）。不指名回合的是用户中断（esc / ctrl+c）：先取消本会话全部 loop 唤醒
    /// （任务表的 owner 是 cron 能力，tools-more.md §18.1），再中止回合或后台工作；空闲时取消了唤醒也算接受。指名的停止
    /// （v5 的 `turn.stop`）被接受时（中止它的回合，或它已经结束）同样取消 loop 唤醒；被拒时（别的回合在跑）什么都不动。
    ///
    /// 修改原因（§10.13，主会话审定）：以前指名的停止不动唤醒，v5 停下一个回合之后 loop 照样到点再开一轮；用户点停止与
    /// TUI 的 Esc 是同一个意图，照 rust 现有的 Esc 行为。
    pub(crate) async fn interrupt(&mut self, target: Option<TurnId>, admission: Admission) -> Ack {
        let cancels = match &target {
            None => true,
            Some(_) => matches!(admission, Admission::Run | Admission::Unchanged),
        };
        let cancelled = if cancels {
            self.deps.cron.cancel_loops(&self.meta.id).await
        } else {
            0
        };
        match admission {
            Admission::Unchanged => {
                return Ack::Accepted {
                    result: CommandResult::Unchanged,
                };
            }
            Admission::Deny(_) if cancelled > 0 => return accepted(),
            Admission::Deny(refusal) => {
                let text = crate::gate::refused(&Command::Interrupt { target }, &refusal);
                return rejected(refusal, text);
            }
            Admission::Run | Admission::Enqueue(_) => {}
        }
        if let Some(running) = &self.running {
            running.abort.abort(AbortReason::User);
            // 回执列出停止之后仍在队列的输入与随这个回合取消的输入（§10.7，02 §4.2）。
            let result = CommandResult::Halted {
                queued: self
                    .inbox
                    .queued()
                    .into_iter()
                    .map(|input| input.command)
                    .collect(),
                cancelled: running
                    .inputs
                    .iter()
                    .map(|(_, input)| input.clone())
                    .collect(),
            };
            // 回合里正在等用户回答的交互一并取消，等待中的工具立即得到错误并结束；后台子代理的询问不属于这个
            // 回合，留着（agents.md §4.1.1）。
            self.cancel_turn_asks();
            return Ack::Accepted { result };
        }
        if let Some(cancel) = self.job.as_ref().and_then(|job| job.cancel.as_ref()) {
            cancel.cancel();
        }
        accepted()
    }

    /// 进程要退出（`Command::Shutdown`，修复线 F38，spec §3.5）：以 `Shutdown` 中止在跑的回合（之后这个回合不再记录），
    /// 取消待回答的交互与可取消的后台工作；此后不再开回合。退出本身不需要中止 query；这里中止只为
    /// 经取消路径回收工具的进程树。
    pub(crate) async fn shutdown(&mut self) -> Ack {
        self.shutting_down = true;
        if let Some(running) = &self.running {
            running.abort.abort(AbortReason::Shutdown);
        } else if let Some(cancel) = self.job.as_ref().and_then(|job| job.cancel.as_ref()) {
            cancel.cancel();
        }
        // 全部待回答的交互：含回合之外的会话级询问（agents.md §4.1.1）。
        self.cancel_pending();
        accepted()
    }

    pub(crate) async fn on_cron_wakeup(&mut self) {
        if !self.busy() {
            self.start_next().await;
        }
    }

    /// 下一个要触发的任务：先排在 actor 里的，没有时向任务表取走此刻到期的（只在空闲时调用）。
    pub(crate) async fn next_cron(&mut self) -> Option<CronFire> {
        if self.cron_fires.is_empty() && self.job.is_none() {
            let due = self.deps.cron.take_due(&self.meta.id).await;
            self.cron_fires.extend(due);
        }
        self.cron_fires.pop_front()
    }
}

#[cfg(test)]
#[path = "cron_tests.rs"]
mod tests;
