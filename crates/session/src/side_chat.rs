//! v5 的侧聊（docs/specs/rust-cli/session-lifecycle.md §10.58）：从这个会话最近一次稳定落盘的边界分出隐藏的子会话，id 由调用方给。
//!
//! 侧聊是一种 fork：存储一次写完（`SessionStore::side_chat`，与 fork 同一个事务写法与逐条克隆），文件不动，附加目录经 `ForkDirectories` 继承。
//! 与 `ForkFromInputAs` 不同的只有复制到哪里：没有回合在跑时全部；回合在跑时带到它开回合的人类输入（含），通知开的回合到它之前——
//! 在跑回合里已经落库的回答与工具结果属于一个还没结束的回答，不带。

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_api::{Ack, CommandResult, Forked};
use rust_cli_kernel_api::store::{ForkReceipt, SideChatBase, SideChatFork, StoreError};

use crate::Inherited;
use crate::actor::{Actor, rejected};
use crate::rewind_plan::selectable;

fn forked(session: SessionId, copied: usize) -> Ack {
    Ack::Accepted {
        result: CommandResult::Forked(Forked {
            session,
            through: None,
            copied,
        }),
    }
}

impl Actor {
    /// `SideChatAs`：准入已经判过（只有关闭中拒绝）。base 在这一次处理里定，在跑回合的下一次提交排在 mailbox 里，不会插进存储的事务。
    pub(crate) async fn side_chat(&self, child: SessionId) -> Ack {
        let fork = SideChatFork {
            parent: self.meta.id.clone(),
            child: child.clone(),
            created_at_ms: self.deps.clock.now_ms(),
            base: self.side_chat_base(),
            selection: self.selection.clone(),
            mode: self.execution.mode,
            plan: self.execution.plan,
        };
        let copied = match self.deps.store.side_chat(fork).await {
            Ok(ForkReceipt::Created { copied }) => copied,
            // 调用方重发（v5）：存储里已经是这次分出的侧聊，什么都没写；目录在第一次时已经记下
            Ok(ForkReceipt::Existing) => return forked(child, 0),
            Err(error @ StoreError::Exists { .. }) => return rejected(Refusal::Unavailable, error),
            // 这个会话还是没落库的草稿（首条输入还没提交）：没有可分的历史
            Err(error @ StoreError::NotFound { .. }) => return rejected(Refusal::NotFound, error),
            Err(error) => return rejected(Refusal::Failed, error),
        };
        // 同 fork：侧聊第一次打开时取走这个会话此刻的附加目录（session-dirs.md §12.5）；另照 v4 带上会话层规则的快照（§10.58，主会话审定），
        // 之后两个会话各改各的
        self.deps.forks.inherit_side_chat(
            child.clone(),
            Inherited {
                directories: self.additional_directories.clone(),
                rules: self.session_rules.clone(),
            },
        );
        forked(child, copied)
    }

    /// 最近一次稳定落盘的边界：按在跑回合的回合记录。记录与回合的第一条消息同一批提交（§10.1），没有记录时这一回合还没有东西落库。
    fn side_chat_base(&self) -> SideChatBase {
        let Some(running) = &self.running else {
            return SideChatBase::All;
        };
        let Some(first) = self
            .turns
            .iter()
            .rev()
            .find(|record| record.turn == running.turn)
            .map(|record| record.first.clone())
        else {
            return SideChatBase::All;
        };
        let by_input = self
            .messages
            .iter()
            .find(|message| message.id() == &first)
            .is_some_and(|message| selectable(message).is_some());
        if by_input {
            SideChatBase::Through(first)
        } else {
            SideChatBase::Before(first)
        }
    }
}

#[cfg(test)]
#[path = "side_chat_tests.rs"]
mod tests;
