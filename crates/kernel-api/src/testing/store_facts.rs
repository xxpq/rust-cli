//! `MemoryStore` 的会话事实（docs/specs/rust-cli/session-facts.md §2、§3 的内存替身）：从提交过的事实重放；写之后事实变了就发出。

use rust_cli_base::sync::LockUnpoisoned;

use super::{MemoryStore, last_title};
use crate::env::WorkspaceRef;
use crate::ids::SessionId;
use crate::message::Message;
use crate::session_facts::{SessionFacts, SessionOutcome, TitleSource};
use crate::store::{Fact, SessionKind, StoreError};

/// 列表会话的种类（node-store 的 `LISTED_TASK_TYPES`）：子代理的子会话与侧聊（session-lifecycle.md §10.58）不进列表。
pub(super) fn listed(kind: SessionKind) -> bool {
    !matches!(kind, SessionKind::Subagent | SessionKind::SideChat)
}

impl MemoryStore {
    /// 会话的事实（session-facts.md §2 的内存替身）：标题取最后一次写入，来源按写法（改名 custom、生成 generated，fork
    /// generated，建会话时带标题 first_input），结局取最后一个 `TurnEnded`；替身没有时钟与消息时刻：删除时刻取活动时间，
    /// 首条输入时刻取第一条人类输入之前的活动时间（`set_updated` 的值）。
    pub(super) fn facts_of(&self, id: &SessionId) -> Option<SessionFacts> {
        let (meta, facts) = self.sessions.lock_unpoisoned().get(id).cloned()?;
        let updated = self
            .updated
            .lock_unpoisoned()
            .get(id)
            .copied()
            .unwrap_or(meta.created_at_ms);
        let mut title_source = if matches!(meta.kind, SessionKind::Fork | SessionKind::SideChat) {
            TitleSource::Generated
        } else if meta.title.is_empty() {
            TitleSource::Default
        } else {
            TitleSource::FirstInput
        };
        let (mut selection, mut execution, mut last_outcome, mut first_input) =
            (None, None, None, None);
        for fact in &facts {
            match fact {
                Fact::TitleChanged { .. } => title_source = TitleSource::Custom,
                Fact::TitleGenerated { .. } => title_source = TitleSource::Generated,
                Fact::ModelSelected { selection: chosen } => selection = Some(chosen.clone()),
                Fact::ExecutionChanged { state } => execution = Some(*state),
                Fact::ModeChanged { mode } => execution = Some((*mode).into()),
                Fact::TurnEnded { status } => {
                    last_outcome = Some(match status {
                        crate::metering::CallStatus::Completed => SessionOutcome::Completed,
                        crate::metering::CallStatus::Cancelled => SessionOutcome::Interrupted,
                        crate::metering::CallStatus::Error => SessionOutcome::Error,
                    });
                }
                Fact::MessageAppended {
                    message: Message::User(user),
                } if first_input.is_none() && crate::user_input::opens_turn(user) => {
                    first_input = Some(updated);
                }
                _ => {}
            }
        }
        Some(SessionFacts {
            title: last_title(&facts).unwrap_or_else(|| meta.title.clone()),
            id: meta.id,
            kind: meta.kind,
            parent: meta.parent,
            workspace: meta.workspace,
            title_source,
            created_at_ms: meta.created_at_ms,
            updated_at_ms: updated,
            last_outcome,
            deleted_at_ms: self.is_deleted(id).then_some(updated),
            first_input_at_ms: first_input,
            migration: None,
            selection,
            execution,
        })
    }

    /// 列表会话（node-store 的口径；替身分得出子代理的子会话与侧聊）的事实。
    pub(super) fn listed_facts(&self, id: &SessionId) -> Option<SessionFacts> {
        self.facts_of(id).filter(|facts| listed(facts.kind))
    }

    /// 一次写：之前与之后的列表会话事实不同就发出（node-store 的同一写法，session-facts.md §3）。
    pub(super) fn tracked<T>(
        &self,
        id: &SessionId,
        write: impl FnOnce() -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let before = self.listed_facts(id);
        let out = write()?;
        if let Some(after) = self
            .listed_facts(id)
            .filter(|after| before.as_ref() != Some(after))
        {
            self.changes.publish(&after);
        }
        Ok(out)
    }

    /// node-store 的同一口径：这个根目录的列表会话，含删除的，按活动时间从新到旧（同一时刻 id 大的在前）。
    pub(super) fn recent_listed(
        &self,
        workspace: &WorkspaceRef,
        limit: usize,
    ) -> Vec<SessionFacts> {
        let ids: Vec<SessionId> = self.sessions.lock_unpoisoned().keys().cloned().collect();
        let mut listed: Vec<SessionFacts> = ids
            .iter()
            .filter_map(|id| self.listed_facts(id))
            .filter(|facts| &facts.workspace == workspace)
            .collect();
        listed.sort_by(|a, b| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then_with(|| b.id.as_str().cmp(a.id.as_str()))
        });
        listed.truncate(limit);
        listed
    }
}
