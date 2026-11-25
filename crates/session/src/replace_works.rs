//! 换掉一条输入时停被截掉的回合里起的后台工作（`ReplaceInput{stop_cut_works}`，docs/specs/rust-cli/session-lifecycle.md §10.55；v5 的编辑与重试
//! 照 v4 传它，`/rewind` 不传）。
//!
//! ```text
//! 截对话之前（同一次处理，回合在跑时是它结束之后的 replace_now）：
//!   截掉的调用 = 目标那条消息起、assistant 消息里的 tool_use
//!   围栏 += 这些调用起的工作（在跑的与结束了的，works.rs）
//!   通道里围栏内的终态通知 → 取走、ack
//!   在跑的 → 停（agents 的 TaskStop 路径）；命令与 monitor 当场结束，子代理在它的 killed 通知到时结束
//! 之后：围栏内的终态通知（迟到的完成、子代理的 killed）→ ack、记结束，不排进通道（不进新分支）
//! ```
//!
//! 为什么在会话 actor：名单要按 owner 的记录定。回合在跑时，它在中止之后、结束之前还能起后台工作（不响应中止的工具跑完了）；名单在回合结束之后、
//! 截断之前定，这些也在里面。迟到的通知同样只有 actor 看得到。

use std::collections::BTreeSet;

use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::{MessageId, ShellTaskId, ToolUseId};
use rust_cli_kernel_api::manage::BackgroundKind;
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::session_api::KernelEvent;

use crate::actor::Actor;
use crate::lane::Waiting;

/// `target` 起的消息里的工具调用（截对话会截掉它们）。
fn cut_tool_uses(messages: &[Message], target: &MessageId) -> BTreeSet<ToolUseId> {
    let Some(at) = messages.iter().position(|message| message.id() == target) else {
        return BTreeSet::new();
    };
    messages[at..]
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => Some(&assistant.content),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

impl Actor {
    /// 截对话之前：围住 `target` 起的调用起的工作，丢掉通道里它们的通知，停掉在跑的。停不了（已经不在跑）的照样围着：它的终态通知不进新分支。
    pub(crate) async fn stop_cut_works(&mut self, target: &MessageId) {
        let cut = cut_tool_uses(&self.messages, target);
        let running = self.works.fence(&cut);
        let dropped = self.inbox.take_notices_of(|id| self.works.fenced(id));
        for notice in &dropped {
            self.ack_notice(notice).await;
        }
        for work in running {
            if work.kind == BackgroundKind::Monitor {
                self.monitors.forget(&ShellTaskId::new(&work.id));
            }
            match self.stop_background(&work.id).await {
                // 停了的命令没有终态通知，不再看的 monitor 也不会再有结束：这里就是它们的结束。子代理在它的 killed 通知到时结束
                Ok(_) if work.kind != BackgroundKind::Agent => {
                    self.work_ended(&work.id, BackgroundStatus::Killed);
                }
                Ok(_) => {}
                // 已经不在跑：它的终态通知在路上或已经处理，围栏照样挡住它
                Err(reason) => {
                    tracing::debug!(work = %work.id, %reason, "a cut background work was not running");
                }
            }
        }
    }

    /// 后台工作的终态通知到了：围栏内的（§10.55）ack、记结束，不排进通道。返回是不是这种。
    pub(crate) async fn fenced_notice(&mut self, notice: &Waiting) -> bool {
        let Some(work) = notice.work_id().filter(|id| self.works.fenced(id)) else {
            return false;
        };
        self.ack_notice(notice).await;
        let status = match notice {
            Waiting::Shell(machine) => machine.notice.status(),
            Waiting::MonitorEnd { notice, .. } => notice.notice.status(),
            Waiting::Agent(agent) => agent.status,
            Waiting::Peer(_) | Waiting::MonitorEvent(_) => return true,
        };
        self.log.append(KernelEvent::BackgroundWork {
            work: work.clone(),
            status,
        });
        self.work_ended(&work, status);
        if matches!(notice, Waiting::Agent(_)) {
            self.works.unfence(&work);
        }
        true
    }
}

#[cfg(test)]
#[path = "replace_works_tests.rs"]
mod tests;
