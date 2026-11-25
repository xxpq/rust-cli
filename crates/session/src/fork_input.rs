//! `/fork`（docs/specs/rust-cli/session-lifecycle.md §6.5，工作线 F75）：从一条可选的用户输入分出新会话。v5 的 `turn.fork`（§10.57）是同一个操作，
//! 新会话的 id 由调用方给、回合在跑时也接受（`ForkFromInputAs`）。
//!
//! 可选的输入与 `/rewind` 相同（`rewind_plan::selectable`）。新会话带到这一轮为止：复制到这一轮最后一条 assistant，
//! 由存储一次写完（`SessionStore::fork`）；文件不动。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::session_api::{Ack, CommandResult, Forked};
use rust_cli_kernel_api::store::{ForkReceipt, SessionFork, StoreError};

use crate::actor::{Actor, rejected};
use crate::rewind_plan::selectable;

/// 复制到哪一条（§6.5.1）：`input` 这一轮（到下一条可选的输入之前）里最后一条 assistant；这一轮没有 assistant
/// 时是 `input` 本身。`input` 不是可选的输入时为 None。
///
/// 续写时补的 `No response requested.`（§6.3.1）不算：它是占位，不是这一轮的回答；还没落库时拿它当复制的终点，
/// 存储找不到这条消息、分叉失败。
pub(crate) fn through(messages: &[Message], input: &MessageId) -> Option<MessageId> {
    let start = messages
        .iter()
        .position(|message| message.id() == input && selectable(message).is_some())?;
    let turn = &messages[start + 1..];
    let end = turn
        .iter()
        .position(|message| selectable(message).is_some())
        .unwrap_or(turn.len());
    let answer = turn[..end].iter().rev().find_map(|message| match message {
        Message::Assistant(assistant) if !assistant.is_resume_sentinel() => {
            Some(assistant.id.clone())
        }
        _ => None,
    });
    Some(answer.unwrap_or_else(|| input.clone()))
}

/// 回合在跑时目标这一轮还没结束（§10.57）。
const TURN_RUNNING: &str = "that turn is still running; fork after it finishes";

impl Actor {
    /// `ForkFromInput`（TUI 的 `/fork`）：新会话的 id 由内核生成。
    pub(crate) async fn fork_from_input(&self, input: MessageId) -> Ack {
        self.fork_into(input, self.deps.ids.session()).await
    }

    /// `ForkFromInput` 与 `ForkFromInputAs`：按内存里的活动历史算出终点，交给存储一次写完新会话（§6.5.2）。`child` 是新会话的 id（`/fork` 由内核
    /// 生成，v5 由调用方给）。准入已经判过：`/fork` 只在空闲时到这里，v5 的回合在跑时也到这里（§10.57）。
    pub(crate) async fn fork_into(&self, input: MessageId, child: SessionId) -> Ack {
        let Some(through) = through(&self.messages, &input) else {
            return rejected(
                Refusal::NotFound,
                format!("{input} is not a message to fork from"),
            );
        };
        if self.reaches_running_turn(&through) {
            return rejected(Refusal::Busy, TURN_RUNNING);
        }
        let fork = SessionFork {
            parent: self.meta.id.clone(),
            child: child.clone(),
            created_at_ms: self.deps.clock.now_ms(),
            through: through.clone(),
            selection: self.selection.clone(),
            mode: self.execution.mode,
            plan: self.execution.plan,
        };
        let copied = match self.deps.store.fork(fork).await {
            Ok(ForkReceipt::Created { copied }) => copied,
            // 调用方重发同一次分叉（v5，§10.57）：存储里已经是这次分叉的会话，什么都没写；目录在第一次时已经记下
            Ok(ForkReceipt::Existing) => {
                return Ack::Accepted {
                    result: CommandResult::Forked(Forked {
                        session: child,
                        through: None,
                        copied: 0,
                    }),
                };
            }
            Err(error @ StoreError::Exists { .. }) => {
                return rejected(Refusal::Unavailable, error);
            }
            Err(error) => return rejected(Refusal::Failed, error),
        };
        // 同 `Fork`：fork 出的会话第一次打开时取走父会话此刻的附加目录（session-dirs.md §12.5）
        self.deps
            .forks
            .inherit(child.clone(), self.additional_directories.clone());
        // 回执的文字（Node 分叉对话的原文）由 `Forked::response` 给出；`through` 与 `copied` 给界面拼中文提示（工作线 F78）。
        Ack::Accepted {
            result: CommandResult::Forked(Forked {
                session: child,
                through: Some(through),
                copied,
            }),
        }
    }

    /// 复制到 `through` 会伸进在跑的回合（§10.57）：在跑回合的第一条消息已经落库、且不在 `through` 之后。人类输入开的回合以它的输入为界，
    /// `through` 停在它之前；通知开的回合不是可选的输入，`through` 会伸进它。在跑的回合还没有消息落库时，历史里的都是之前的回合。
    fn reaches_running_turn(&self, through: &MessageId) -> bool {
        let Some(running) = &self.running else {
            return false;
        };
        let Some(first) = self
            .turns
            .iter()
            .rev()
            .find(|record| record.turn == running.turn)
            .map(|record| &record.first)
        else {
            return false;
        };
        let position = |id: &MessageId| self.messages.iter().position(|message| message.id() == id);
        position(first)
            .zip(position(through))
            .is_some_and(|(start, end)| end >= start)
    }
}

#[cfg(test)]
#[path = "fork_input_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "fork_as_tests.rs"]
mod as_tests;
