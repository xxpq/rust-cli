//! `MemoryStore` 的永久删除（node-store.md §5.3 的内存替身，从 store.rs 分出，800 行上限）：自有后代是 `parent` 指向树里的子代理与侧聊，
//! fork 是父会话在树里的别的会话；没有 artifact 引用（替身的事实里没有附件的 URI）；文件只有测试放进去的；凭据照 node-store 的规则
//! （有凭据时范围是凭据里的会话，删完改成 deleted）。

use async_trait::async_trait;
use rust_cli_base::sync::LockUnpoisoned;

use super::MemoryStore;
use crate::deletion::{
    BeginOutcome, DeletionBegin, DeletionScope, Receipt, SessionFile, SessionPurge, SessionStamp,
    TitledSession,
};
use crate::ids::SessionId;
use crate::store::{SessionKind, StoreError};

/// 一个根的删除凭据。
#[derive(Debug, Clone)]
pub(super) struct MemoryReceipt {
    sessions: Vec<SessionId>,
    deleted: bool,
}

impl MemoryStore {
    /// 给会话放一个文件（`session_files` 原样给出，按相对路径排序）。
    pub fn add_file(&self, session: &SessionId, file: SessionFile) {
        let mut files = self.files.lock_unpoisoned();
        let list = files.entry(session.clone()).or_default();
        list.push(file);
        list.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    }

    /// 会话现在的文件。
    pub fn files_of(&self, session: &SessionId) -> Vec<SessionFile> {
        self.files
            .lock_unpoisoned()
            .get(session)
            .cloned()
            .unwrap_or_default()
    }

    /// 会话行还在（永久删除之后没有，软删除之后仍有）。
    pub fn has_row(&self, session: &SessionId) -> bool {
        self.sessions.lock_unpoisoned().contains_key(session)
    }

    /// 根的删除凭据的状态。
    pub fn receipt(&self, root: &SessionId) -> Option<Receipt> {
        self.receipts.lock_unpoisoned().get(root).map(|receipt| {
            if receipt.deleted {
                Receipt::Deleted
            } else {
                Receipt::Deleting
            }
        })
    }

    fn stamps(&self, tree: &[SessionId]) -> Vec<SessionStamp> {
        let sessions = self.sessions.lock_unpoisoned();
        let updated = self.updated.lock_unpoisoned();
        tree.iter()
            .filter_map(|id| {
                let (meta, _) = sessions.get(id)?;
                Some(SessionStamp {
                    session: id.clone(),
                    updated_ms: updated.get(id).copied().unwrap_or(meta.created_at_ms),
                    deleted_ms: None,
                })
            })
            .collect()
    }
}

#[async_trait]
impl SessionPurge for MemoryStore {
    async fn deletion_scope(&self, root: &SessionId) -> Result<DeletionScope, StoreError> {
        let receipt = self.receipts.lock_unpoisoned().get(root).cloned();
        let sessions = self.sessions.lock_unpoisoned();
        let workspace = sessions.get(root).map(|(meta, _)| meta.workspace.clone());
        let tree = match &receipt {
            Some(receipt) => receipt.sessions.clone(),
            None if workspace.is_none() => return Ok(DeletionScope::default()),
            None => {
                let owned = |kind: SessionKind| {
                    matches!(kind, SessionKind::Subagent | SessionKind::SideChat)
                };
                let mut tree = vec![root.clone()];
                let mut index = 0;
                while index < tree.len() {
                    let parent = tree[index].clone();
                    let mut children: Vec<SessionId> = sessions
                        .values()
                        .filter(|(child, _)| {
                            child.parent.as_ref() == Some(&parent) && owned(child.kind)
                        })
                        .map(|(child, _)| child.id.clone())
                        .filter(|id| !tree.contains(id))
                        .collect();
                    children.sort();
                    tree.extend(children);
                    index += 1;
                }
                tree
            }
        };
        let deleted = self.deleted.lock_unpoisoned();
        let mut forks: Vec<TitledSession> = sessions
            .values()
            .filter(|(child, _)| {
                child
                    .parent
                    .as_ref()
                    .is_some_and(|parent| tree.contains(parent))
                    && !tree.contains(&child.id)
                    && !deleted.contains(&child.id)
            })
            .map(|(child, _)| TitledSession {
                session: child.id.clone(),
                title: child.title.clone(),
            })
            .collect();
        forks.sort_by(|a, b| a.session.cmp(&b.session));
        drop((sessions, deleted));
        Ok(DeletionScope {
            stamps: self.stamps(&tree),
            workspace,
            sessions: tree,
            forks,
            references: Vec::new(),
            pending_work: false,
            receipt: self.receipt(root),
        })
    }

    async fn session_files(&self, session: &SessionId) -> Result<Vec<SessionFile>, StoreError> {
        Ok(self.files_of(session))
    }

    async fn begin_deletion(
        &self,
        root: &SessionId,
        begin: &DeletionBegin,
    ) -> Result<BeginOutcome, StoreError> {
        if self.receipts.lock_unpoisoned().contains_key(root) {
            return Ok(BeginOutcome::Admitted);
        }
        let sessions: Vec<SessionId> = begin
            .stamps
            .iter()
            .map(|stamp| stamp.session.clone())
            .collect();
        if self.stamps(&sessions) != begin.stamps {
            return Ok(BeginOutcome::Changed);
        }
        self.receipts.lock_unpoisoned().insert(
            root.clone(),
            MemoryReceipt {
                sessions,
                deleted: false,
            },
        );
        Ok(BeginOutcome::Admitted)
    }

    async fn remove_session_files(
        &self,
        session: &SessionId,
        keep: &[String],
    ) -> Result<(), StoreError> {
        if let Some(files) = self.files.lock_unpoisoned().get_mut(session) {
            files.retain(|file| file.artifact && keep.iter().any(|id| file.name.contains(id)));
        }
        Ok(())
    }

    async fn complete_deletion(&self, root: &SessionId) -> Result<(), StoreError> {
        let mut receipts = self.receipts.lock_unpoisoned();
        let Some(receipt) = receipts.get_mut(root) else {
            return Ok(());
        };
        let mut rows = self.sessions.lock_unpoisoned();
        let mut deleted = self.deleted.lock_unpoisoned();
        for session in &receipt.sessions {
            rows.remove(session);
            deleted.remove(session);
        }
        receipt.deleted = true;
        Ok(())
    }
}
