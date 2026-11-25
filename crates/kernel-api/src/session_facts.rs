//! 会话列表的事实（docs/specs/rust-cli/session-facts.md §2、§3）：一个会话行的持久事实，与它的变化。
//!
//! 内核与前端无关：这里只有存储里已有的事实（Node 的会话行与它的配置记录）；前端怎么投影它们是前端的事。查询与变化在
//! [`crate::store::StoreQueries`]。

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::Mutex;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::env::WorkspaceRef;
use crate::ids::SessionId;
use crate::model::ModelSelection;
use crate::permission::ExecutionState;
use crate::store::SessionKind;

/// 会话标题的来源（Node `session.title_source`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleSource {
    /// 还没有标题。
    #[default]
    Default,
    /// 首条输入的标题（Node `titleFromInput`）。
    FirstInput,
    /// 生成的标题，或 fork 的 `Fork of …`。
    Generated,
    /// 用户改的名（之后不再自动生成）。
    Custom,
}

/// 最后一轮的结局（Node `session.last_outcome`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOutcome {
    Completed,
    Interrupted,
    Error,
}

/// 会话的历史是从别的工具导入的（Node 导入的历史在消息的 `metadata.migrationSource` 或固定的消息 ID 里）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationSource {
    ImportedHistory,
}

/// 一个会话行的持久事实（session-facts.md §2 的表）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFacts {
    pub id: SessionId,
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    pub workspace: WorkspaceRef,
    pub title: String,
    pub title_source: TitleSource,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_outcome: Option<SessionOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at_ms: Option<u64>,
    /// 首条真实用户输入的时刻；还没有输入时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_input_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<MigrationSource>,
    /// 会话记下的模型选择（不带团队项目）；没有时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ModelSelection>,
    /// 会话记下的权限模式与 plan 开关；没有时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionState>,
}

/// 事实变化的流（[`crate::store::StoreQueries::session_fact_changes`]）。
pub type FactStream = BoxStream<'static, SessionFacts>;

/// 事实变化的发布端（session-facts.md §3）：存储适配器持有一份，事务提交之后按提交的顺序 [`Self::publish`]。
/// 订阅者每个一个无界通道：慢的订阅者不丢变化；丢掉接收端的订阅者在下一次发布时去掉。
#[derive(Default)]
pub struct FactChanges {
    watchers: Mutex<Vec<mpsc::UnboundedSender<SessionFacts>>>,
}

impl FactChanges {
    /// 之后发布的变化（订阅之前的不补：先订阅、再读种子，session-facts.md §3.1）。
    pub fn subscribe(&self) -> FactStream {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.watchers.lock_unpoisoned().push(sender);
        futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|facts| (facts, receiver))
        })
        .boxed()
    }

    pub fn publish(&self, facts: &SessionFacts) {
        self.watchers
            .lock_unpoisoned()
            .retain(|watcher| watcher.send(facts.clone()).is_ok());
    }
}

#[cfg(test)]
#[path = "session_facts_tests.rs"]
mod tests;
