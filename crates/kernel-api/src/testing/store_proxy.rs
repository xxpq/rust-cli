//! 全委托的存储代理（审查 K6 / K7）：三个会话端口的方法全部交给内层的存储；测试只写关心的那一步（停住、计数、让一次
//! 失败、改冷加载的结果）。以前每个测试各写一个包装，只转发 4–7 个方法，其余落到 trait 的缺省 `Unsupported`。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::deletion::{BeginOutcome, DeletionBegin, DeletionScope, SessionFile, SessionPurge};
use crate::env::WorkspaceRef;
use crate::ids::SessionId;
use crate::metering::{ActivityQuery, ActivityStats, RequestStats, SessionMetering};
use crate::permission::{PermissionMode, ProjectRules};
use crate::session_facts::SessionFacts;
use crate::store::{
    CommitReceipt, FactBatch, ForkReceipt, FrontendRecord, LoadedSession, LocalSettings,
    ProjectRulesChange, SessionFilter, SessionFork, SessionMeta, SessionPage, SessionQuery,
    SessionStore, SessionSummary, SideChatFork, Store, StoreError, StoreQueries,
};
use crate::workspace_trust::TrustKey;

/// 会话生命周期与几条查询的一次调用（[`StoreHooks::before`] 的参数）。
#[derive(Clone, Copy)]
pub enum StoreCall<'a> {
    Create(&'a SessionMeta),
    Commit(&'a SessionId, &'a FactBatch),
    Load(&'a SessionId),
    List,
    Entries(&'a SessionId),
    Fork(&'a SessionFork),
    SideChat(&'a SideChatFork),
    QuerySessions,
    Rename(&'a SessionId),
    Delete(&'a SessionId),
    /// 永久删除的删完（[`crate::deletion::SessionPurge::complete_deletion`]）：参数是根。
    CompleteDeletion(&'a SessionId),
    /// 库里的子代理（[`StoreQueries::subagents`]）。
    Subagents(&'a SessionId),
    /// 会话事实（[`StoreQueries::session_facts`]）。
    SessionFacts(&'a [SessionId]),
    /// 历史的一段（[`StoreQueries::history`]）：会话与给出的存储消息。
    History(&'a SessionId, &'a [crate::ids::MessageId]),
    /// 范围读（[`StoreQueries::range`]）：会话与选段。
    Range(&'a SessionId, &'a crate::store::HistorySpan),
}

/// 代理给测试的插入点；缺省什么都不做（只在测试里，不掩盖产品代码的漏转发）。
#[async_trait]
pub trait StoreHooks: Send + Sync {
    /// 会话生命周期的调用交给内层之前：可以等、可以计数；返回错误时这次调用以它失败。
    async fn before(&self, _call: StoreCall<'_>) -> Result<(), StoreError> {
        Ok(())
    }

    /// 冷加载的结果交出之前。
    fn loaded(&self, _session: &SessionId, _loaded: &mut LoadedSession) {}
}

#[async_trait]
impl<T: StoreHooks + ?Sized> StoreHooks for Arc<T> {
    async fn before(&self, call: StoreCall<'_>) -> Result<(), StoreError> {
        (**self).before(call).await
    }

    fn loaded(&self, session: &SessionId, loaded: &mut LoadedSession) {
        (**self).loaded(session, loaded);
    }
}

/// 全委托的存储。
pub struct StoreProxy<H> {
    inner: Arc<dyn Store>,
    hooks: H,
}

impl<H: StoreHooks> StoreProxy<H> {
    pub fn new(inner: Arc<dyn Store>, hooks: H) -> Self {
        Self { inner, hooks }
    }
}

#[async_trait]
impl<H: StoreHooks> SessionStore for StoreProxy<H> {
    async fn create(
        &self,
        meta: SessionMeta,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        self.hooks.before(StoreCall::Create(&meta)).await?;
        self.inner.create(meta, batch).await
    }

    async fn commit(
        &self,
        session: &SessionId,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        self.hooks
            .before(StoreCall::Commit(session, &batch))
            .await?;
        self.inner.commit(session, batch).await
    }

    async fn load(&self, session: &SessionId) -> Result<LoadedSession, StoreError> {
        self.hooks.before(StoreCall::Load(session)).await?;
        let mut loaded = self.inner.load(session).await?;
        self.hooks.loaded(session, &mut loaded);
        Ok(loaded)
    }

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
        self.hooks.before(StoreCall::List).await?;
        self.inner.list(filter).await
    }

    async fn entries(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<(String, Value)>, StoreError> {
        self.hooks.before(StoreCall::Entries(session)).await?;
        self.inner.entries(session, kind).await
    }

    async fn fork(&self, fork: SessionFork) -> Result<ForkReceipt, StoreError> {
        self.hooks.before(StoreCall::Fork(&fork)).await?;
        self.inner.fork(fork).await
    }

    async fn side_chat(&self, fork: SideChatFork) -> Result<ForkReceipt, StoreError> {
        self.hooks.before(StoreCall::SideChat(&fork)).await?;
        self.inner.side_chat(fork).await
    }

    async fn query_sessions(&self, query: SessionQuery) -> Result<SessionPage, StoreError> {
        self.hooks.before(StoreCall::QuerySessions).await?;
        self.inner.query_sessions(query).await
    }

    async fn rename(&self, session: &SessionId, title: &str) -> Result<(), StoreError> {
        self.hooks.before(StoreCall::Rename(session)).await?;
        self.inner.rename(session, title).await
    }

    async fn delete(&self, session: &SessionId) -> Result<(), StoreError> {
        self.hooks.before(StoreCall::Delete(session)).await?;
        self.inner.delete(session).await
    }
}

#[async_trait]
impl<H: StoreHooks> LocalSettings for StoreProxy<H> {
    async fn project_rules(&self, workspace: &WorkspaceRef) -> Result<ProjectRules, StoreError> {
        self.inner.project_rules(workspace).await
    }

    async fn change_project_rules(
        &self,
        workspace: &WorkspaceRef,
        change: ProjectRulesChange,
    ) -> Result<ProjectRules, StoreError> {
        self.inner.change_project_rules(workspace, change).await
    }

    async fn remember_directories(
        &self,
        workspace: &WorkspaceRef,
        directories: Vec<crate::env::EnvPath>,
    ) -> Result<(), StoreError> {
        self.inner
            .remember_directories(workspace, directories)
            .await
    }

    async fn session_rules(&self, session: &SessionId) -> Result<Option<ProjectRules>, StoreError> {
        self.inner.session_rules(session).await
    }

    async fn project_mode(
        &self,
        workspace: &WorkspaceRef,
    ) -> Result<Option<PermissionMode>, StoreError> {
        self.inner.project_mode(workspace).await
    }

    async fn trusted_workspace(
        &self,
        candidates: &[TrustKey],
    ) -> Result<Option<TrustKey>, StoreError> {
        self.inner.trusted_workspace(candidates).await
    }

    async fn trust_workspace(&self, key: &TrustKey) -> Result<(), StoreError> {
        self.inner.trust_workspace(key).await
    }
}

#[async_trait]
impl<H: StoreHooks> StoreQueries for StoreProxy<H> {
    async fn metering(&self, session: &SessionId) -> Result<SessionMetering, StoreError> {
        self.inner.metering(session).await
    }

    async fn request_stats(&self, session: &SessionId) -> Result<RequestStats, StoreError> {
        self.inner.request_stats(session).await
    }

    async fn activity(&self, query: ActivityQuery) -> Result<ActivityStats, StoreError> {
        self.inner.activity(query).await
    }

    async fn subagents(
        &self,
        session: &SessionId,
    ) -> Result<Vec<crate::agents::StoredSubagent>, StoreError> {
        self.hooks.before(StoreCall::Subagents(session)).await?;
        self.inner.subagents(session).await
    }

    async fn put_record(
        &self,
        session: &SessionId,
        record: &FrontendRecord,
    ) -> Result<(), StoreError> {
        self.inner.put_record(session, record).await
    }

    async fn records(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<FrontendRecord>, StoreError> {
        self.inner.records(session, kind).await
    }

    async fn session_facts(&self, sessions: &[SessionId]) -> Result<Vec<SessionFacts>, StoreError> {
        self.hooks.before(StoreCall::SessionFacts(sessions)).await?;
        self.inner.session_facts(sessions).await
    }

    async fn branch_cut(&self, session: &SessionId) -> Result<Option<String>, StoreError> {
        self.inner.branch_cut(session).await
    }

    async fn history(
        &self,
        session: &SessionId,
        records: &[crate::ids::MessageId],
    ) -> Result<crate::store::HistorySlice, StoreError> {
        self.hooks
            .before(StoreCall::History(session, records))
            .await?;
        self.inner.history(session, records).await
    }

    async fn range(
        &self,
        session: &SessionId,
        span: crate::store::HistorySpan,
    ) -> Result<crate::store::HistoryRange, StoreError> {
        self.hooks.before(StoreCall::Range(session, &span)).await?;
        self.inner.range(session, span).await
    }

    async fn recent_session_facts(
        &self,
        workspace: &WorkspaceRef,
        limit: usize,
    ) -> Result<Vec<SessionFacts>, StoreError> {
        self.inner.recent_session_facts(workspace, limit).await
    }

    fn session_fact_changes(&self) -> crate::session_facts::FactStream {
        self.inner.session_fact_changes()
    }
}

#[async_trait]
impl<H: StoreHooks> SessionPurge for StoreProxy<H> {
    async fn deletion_scope(&self, root: &SessionId) -> Result<DeletionScope, StoreError> {
        self.inner.deletion_scope(root).await
    }

    async fn session_files(&self, session: &SessionId) -> Result<Vec<SessionFile>, StoreError> {
        self.inner.session_files(session).await
    }

    async fn remove_session_files(
        &self,
        session: &SessionId,
        keep: &[String],
    ) -> Result<(), StoreError> {
        self.inner.remove_session_files(session, keep).await
    }

    async fn begin_deletion(
        &self,
        root: &SessionId,
        begin: &DeletionBegin,
    ) -> Result<BeginOutcome, StoreError> {
        self.inner.begin_deletion(root, begin).await
    }

    async fn complete_deletion(&self, root: &SessionId) -> Result<(), StoreError> {
        self.hooks.before(StoreCall::CompleteDeletion(root)).await?;
        self.inner.complete_deletion(root).await
    }
}
