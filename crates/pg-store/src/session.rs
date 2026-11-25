//! `SessionStore` 的云端实现（pg-store.md §3、§4、§12）：写入、冷读取、fork 与查询都走 node-records 的编排，行是与本机
//! 相同的 Node 行；这里只负责用户隔离、项目绑定与事务。
use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::metering::{ActivityQuery, ActivityStats, RequestStats, SessionMetering};
use rust_cli_kernel_api::permission::{PermissionMode, ProjectRules};
use rust_cli_kernel_api::store::*;
use rust_cli_kernel_api::workspace_trust::TrustKey;
use rust_cli_node_records::rewind::Active;
use rust_cli_node_records::rows::Rows;
use rust_cli_node_records::{facts, frontend_records, load, project_mode, rust_entries};
use rust_cli_node_records::{project_rules, query, workspace_trust};
use sea_orm::{ColumnTrait, QueryFilter};
use serde_json::Value;

use crate::entities::session as session_entity;
use crate::orm::error as orm_error;
use crate::rows::PgRows;
use crate::transaction::Tx;
use crate::{PgSessionStore, number, storage};

/// 新会话行写入的版本（与 CLI 的 Node 会话相同的列；云端没有 CLI 版本号）。
const VERSION: &str = "rust-cli-cloud";

impl PgSessionStore {
    /// 在阻塞线程里对同一个事务执行 node-records 的编排（[`PgRows`]），事务交回调用方继续提交。失败时事务被丢弃、
    /// 回滚。
    pub(crate) async fn with_rows<T: Send + 'static>(
        &self,
        tx: Tx,
        work: impl FnOnce(&PgRows<'_>) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<(Tx, T), StoreError> {
        let store = self.clone();
        let runtime = tokio::runtime::Handle::current();
        let (tx, result) = rust_cli_base::blocking(move || {
            let result = work(&PgRows {
                store: &store,
                tx: &tx,
                runtime,
            });
            (tx, result)
        })
        .await
        .map_err(|_| storage("session store task failed"))?;
        Ok((tx, result?))
    }

    fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    /// 编排给 part、turn 的随机部分：与本机的会话库同一个来源（`UuidSource`）。
    ///
    /// 修 bug（合入 v5 时，`pg_node_parity` 查出）：以前借 `Ids::command()` 取 uuid，那时它是不带前缀的 uuid；v5 把命令 id
    /// 改成 UUIDv7（session-lifecycle.md §10.2，K-A3），不再是 uuid 源给的原值，云端与本机写出的 fork 副本等记录的 id 不再
    /// 相同。根因是借用了一个语义不同的方法；现在 `bind` 直接收记录用的 uuid 来源。
    fn uuid(&self) -> Arc<dyn Fn() -> String + Send + Sync> {
        let uuids = self.uuids.clone();
        Arc::new(move || uuids.uuid())
    }

    /// 本用户在这个项目里的信任记录键：信任按用户与项目隔离（§2），共用 Node 的 `local_setting` 行。
    fn trust_key(&self, project: &str, key: &TrustKey) -> TrustKey {
        TrustKey::new(format!(
            "{}:{project}:{}",
            self.access.user_id(),
            key.as_str()
        ))
    }

    /// 新会话：项目来自绑定（不按路径计算），子会话必须与父会话同项目。
    async fn insert_session(&self, tx: Tx, meta: &SessionMeta) -> Result<Tx, StoreError> {
        let project = self.project(&meta.workspace)?.id.clone();
        let input = load::create_input(meta, project.clone(), VERSION);
        let parent = meta.parent.clone();
        let now = number(self.now_ms())?;
        let (tx, ()) = self
            .with_rows(tx, move |rows| {
                if let Some(parent) = &parent {
                    let owner = facts::live_session(rows, parent)?;
                    let same = rows
                        .session_project_id(&owner.id)?
                        .is_some_and(|id| id == project);
                    if !same {
                        return Err(storage("child session must belong to the parent project"));
                    }
                }
                rows.create_session(&input, now)
            })
            .await?;
        Ok(tx)
    }

    /// 应用一批事实（node-records），再处理只在云端的输入提升（§4）。
    async fn apply(
        &self,
        tx: Tx,
        session: &SessionId,
        batch: &FactBatch,
    ) -> Result<(Tx, CommitReceipt), StoreError> {
        let (owned_session, owned_batch) = (session.clone(), batch.clone());
        let (now, uuid) = (self.now_ms(), self.uuid());
        let (tx, receipt) = self
            .with_rows(tx, move |rows| {
                facts::apply_batch(rows, &owned_session, &owned_batch, (now, &*uuid))
            })
            .await?;
        for fact in &batch.facts {
            if let Fact::InputsPromoted { commands, message } = fact {
                self.promote_inputs(&tx, session, commands, message).await?;
            }
        }
        Ok((tx, receipt))
    }

    /// 只读事务里的编排。
    async fn read<T: Send + 'static>(
        &self,
        work: impl FnOnce(&PgRows<'_>) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let tx = self.begin(true).await?;
        let (tx, result) = self.with_rows(tx, work).await?;
        tx.commit().await.map_err(orm_error)?;
        Ok(result)
    }

    /// 绑定项目里未删除的会话；其他项目或租户的会话当作不存在。
    async fn require_live(&self, session: &SessionId) -> Result<(), StoreError> {
        let mut tx = self.begin(true).await?;
        self.live(&mut tx, session, false).await?;
        tx.commit().await.map_err(orm_error)
    }
}

/// 一条会话记录删没删（[`PgSessionStore::check_delete`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteState {
    Live,
    Deleted,
}

impl PgSessionStore {
    /// 删除前看这条记录（pg-store.md §6）：不存在或不在绑定的项目里时 `NotFound`；已删除的返回 `Deleted`，`delete` 据此
    /// 什么都不做（幂等）。
    pub(crate) async fn check_delete(
        &self,
        session: &SessionId,
    ) -> Result<DeleteState, StoreError> {
        let tx = self.begin(true).await?;
        let row = self
            .find::<session_entity::Entity>()
            .filter(session_entity::Column::Id.eq(session.as_str()))
            .filter(session_entity::Column::ProjectId.is_in(self.project_ids()))
            .one(&tx)
            .await
            .map_err(orm_error)?
            .ok_or_else(|| crate::not_found(session))?;
        tx.commit().await.map_err(orm_error)?;
        Ok(if row.time_deleted.is_some() {
            DeleteState::Deleted
        } else {
            DeleteState::Live
        })
    }
}

#[async_trait]
impl SessionStore for PgSessionStore {
    async fn create(
        &self,
        meta: SessionMeta,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        let tx = self.begin(false).await?;
        let mut tx = self.insert_session(tx, &meta).await?;
        // 没有初始事实时只建会话、不提交（`SessionStore::create` 的约定，与本机一致）。
        let receipt = if batch.facts.is_empty() && batch.command.is_none() {
            CommitReceipt { sequence: 0 }
        } else {
            let (next, receipt) = self.apply(tx, &meta.id, &batch).await?;
            tx = next;
            receipt
        };
        self.finish(tx).await?;
        Ok(receipt)
    }

    async fn commit(
        &self,
        session: &SessionId,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        let mut tx = self.begin(false).await?;
        // 草稿落库（session-facts.md §4）：第一项是 `SessionCreated` 时同一个事务里先建会话行（同 `create`），其余的事实随后
        // 应用（node-records 的 `apply_batch` 跳过第一项）；否则会话必须已在绑定的项目里、没删除。
        match facts::created(&batch) {
            Some((meta, _)) => tx = self.insert_session(tx, meta).await?,
            None => self.live(&mut tx, session, true).await?,
        }
        let (tx, receipt) = self.apply(tx, session, &batch).await?;
        self.finish(tx).await?;
        Ok(receipt)
    }

    async fn load(&self, session: &SessionId) -> Result<LoadedSession, StoreError> {
        self.require_live(session).await?;
        let owned = session.clone();
        let (mut loaded, checkpoints) = self.read(move |rows| load::load(rows, &owned)).await?;
        // 云端只有本工程写的 checkpoint（Node 的 checkpoint 要读本机产物目录）。
        let rust = |all: Vec<Active>| {
            all.into_iter()
                .filter_map(|active| match active {
                    Active::Rust(checkpoint) => Some(checkpoint),
                    Active::Node(_) => None,
                })
                .collect()
        };
        loaded.checkpoints = rust(checkpoints.active);
        loaded.reverted = rust(checkpoints.reverted);
        Ok(loaded)
    }

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
        if filter.limit == 0 {
            return Ok(Vec::new());
        }
        self.project(&filter.workspace)?;
        self.read(move |rows| load::list(rows, &filter)).await
    }

    async fn entries(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<(String, Value)>, StoreError> {
        self.require_live(session).await?;
        let (session, kind) = (session.as_str().to_owned(), kind.to_owned());
        self.read(move |rows| rust_entries::kind_entries(rows, &session, &kind))
            .await
    }

    async fn fork(&self, fork: SessionFork) -> Result<ForkReceipt, StoreError> {
        let mut tx = self.begin(false).await?;
        self.live(&mut tx, &fork.parent, true).await?;
        let (now, uuid, owned) = (self.now_ms(), self.uuid(), fork.clone());
        let (tx, receipt) = self
            .with_rows(tx, move |rows| {
                rust_cli_node_records::fork::fork(rows, &owned, (now, &*uuid))
            })
            .await?;
        self.finish(tx).await?;
        Ok(receipt)
    }

    /// 侧聊的新会话（session-lifecycle.md §10.58）：与 fork 同一个事务写法，记录同 node-records 的 `side_chat`。
    async fn side_chat(&self, fork: SideChatFork) -> Result<ForkReceipt, StoreError> {
        let mut tx = self.begin(false).await?;
        self.live(&mut tx, &fork.parent, true).await?;
        let (now, uuid, owned) = (self.now_ms(), self.uuid(), fork.clone());
        let (tx, receipt) = self
            .with_rows(tx, move |rows| {
                rust_cli_node_records::side_chat::side_chat(rows, &owned, (now, &*uuid))
            })
            .await?;
        self.finish(tx).await?;
        Ok(receipt)
    }

    async fn query_sessions(&self, request: SessionQuery) -> Result<SessionPage, StoreError> {
        if !self
            .projects
            .iter()
            .any(|project| project.workspace.environment == request.environment)
        {
            return Ok(SessionPage {
                sessions: Vec::new(),
                more: false,
            });
        }
        self.read(move |rows| query::query(rows, &request)).await
    }

    async fn rename(&self, session: &SessionId, title: &str) -> Result<(), StoreError> {
        let mut tx = self.begin(false).await?;
        self.live(&mut tx, session, true).await?;
        let (owned, title, now) = (session.clone(), title.to_owned(), self.now_ms());
        let (tx, ()) = self
            .with_rows(tx, move |rows| query::rename(rows, &owned, &title, now))
            .await?;
        self.finish(tx).await
    }

    async fn delete(&self, session: &SessionId) -> Result<(), StoreError> {
        if self.check_delete(session).await? == DeleteState::Deleted {
            return Ok(());
        }
        let mut tx = self.begin(false).await?;
        self.live(&mut tx, session, true).await?;
        let (owned, now) = (session.clone(), self.now_ms());
        let (tx, ()) = self
            .with_rows(tx, move |rows| query::delete(rows, &owned, now))
            .await?;
        self.finish(tx).await
    }
}

#[async_trait]
impl LocalSettings for PgSessionStore {
    async fn project_rules(&self, workspace: &WorkspaceRef) -> Result<ProjectRules, StoreError> {
        let project = self.project(workspace)?.id.clone();
        self.read(move |rows| project_rules::read(rows, &project))
            .await
    }

    /// `/permissions` 的修改：共享项目规则集只有 owner / admin 能改（§6 的权限表）。
    async fn change_project_rules(
        &self,
        workspace: &WorkspaceRef,
        change: ProjectRulesChange,
    ) -> Result<ProjectRules, StoreError> {
        let project = self.project(workspace)?.id.clone();
        let tx = self.begin(false).await?;
        let now = self.now_ms();
        let (tx, rules) = self
            .with_rows(tx, move |rows| {
                project_rules::change(rows, &project, now, &change)
            })
            .await?;
        self.finish(tx).await?;
        Ok(rules)
    }

    /// 没有会话时记住附加目录（permission-updates.md §6.1）：写绑定项目的规则集，同 `/permissions` 的事务写法。
    async fn remember_directories(
        &self,
        workspace: &WorkspaceRef,
        directories: Vec<rust_cli_kernel_api::env::EnvPath>,
    ) -> Result<(), StoreError> {
        let project = self.project(workspace)?.id.clone();
        let tx = self.begin(false).await?;
        let now = self.now_ms();
        let (tx, ()) = self
            .with_rows(tx, move |rows| {
                project_rules::remember_in(rows, &project, now, &directories)
            })
            .await?;
        self.finish(tx).await
    }

    async fn session_rules(&self, session: &SessionId) -> Result<Option<ProjectRules>, StoreError> {
        self.require_live(session).await?;
        let session = session.as_str().to_owned();
        // 云端每次执行前都要拿到生效的规则（runtime `refresh_cloud`）：群会话（`scope = session`）用会话自己的规则集，
        // 其余用会话所在项目的共享规则集（与 Node 读规则的口径相同）。
        self.read(move |rows| {
            if let Some(rules) = project_rules::session_read(rows, &session)? {
                return Ok(Some(rules));
            }
            let project = rows
                .session_project_id(&session)?
                .ok_or_else(|| storage("session project missing"))?;
            project_rules::read(rows, &project).map(Some)
        })
        .await
    }

    async fn project_mode(
        &self,
        workspace: &WorkspaceRef,
    ) -> Result<Option<PermissionMode>, StoreError> {
        let project = self.project(workspace)?.id.clone();
        self.read(move |rows| project_mode::read(rows, &project))
            .await
    }

    async fn trusted_workspace(
        &self,
        candidates: &[TrustKey],
    ) -> Result<Option<TrustKey>, StoreError> {
        let keyed: Vec<(TrustKey, TrustKey)> = candidates
            .iter()
            .flat_map(|key| {
                self.projects
                    .iter()
                    .map(|project| (self.trust_key(&project.id, key), key.clone()))
            })
            .collect();
        let mine: Vec<TrustKey> = keyed.iter().map(|(mine, _)| mine.clone()).collect();
        let found = self
            .read(move |rows| workspace_trust::find(rows, &mine))
            .await?;
        Ok(found.and_then(|found| {
            keyed
                .into_iter()
                .find(|(mine, _)| *mine == found)
                .map(|(_, key)| key)
        }))
    }

    /// 信任只绑定单个项目的宿主上记录（§2：不能用路径猜测项目身份）。
    async fn trust_workspace(&self, key: &TrustKey) -> Result<(), StoreError> {
        if self.projects.len() != 1 {
            return Err(storage("workspace trust requires a single project binding"));
        }
        let tx = self.begin(false).await?;
        let (key, now) = (self.trust_key(&self.projects[0].id, key), self.now_ms());
        let (tx, ()) = self
            .with_rows(tx, move |rows| workspace_trust::record(rows, &key, now))
            .await?;
        self.finish(tx).await
    }
}

#[async_trait]
impl StoreQueries for PgSessionStore {
    async fn metering(&self, session: &SessionId) -> Result<SessionMetering, StoreError> {
        self.session_metering(session).await
    }

    async fn request_stats(&self, session: &SessionId) -> Result<RequestStats, StoreError> {
        self.stats(session).await
    }

    async fn activity(&self, query: ActivityQuery) -> Result<ActivityStats, StoreError> {
        self.activity_stats(query).await
    }

    async fn put_record(
        &self,
        session: &SessionId,
        record: &FrontendRecord,
    ) -> Result<(), StoreError> {
        let mut tx = self.begin(false).await?;
        self.live(&mut tx, session, true).await?;
        let (owned, record, now) = (session.as_str().to_owned(), record.clone(), self.now_ms());
        let (tx, ()) = self
            .with_rows(tx, move |rows| {
                frontend_records::put(rows, &owned, &record, now)
            })
            .await?;
        self.finish(tx).await
    }

    async fn records(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<FrontendRecord>, StoreError> {
        self.require_live(session).await?;
        let (session, kind) = (session.as_str().to_owned(), kind.to_owned());
        self.read(move |rows| frontend_records::list(rows, &session, &kind))
            .await
    }

    /// 这些会话的事实（session-facts.md §2）：与本机同一份读法（node-records 的 `session_facts`），只看绑定项目里的会话。
    /// 内核建会话前用它判断调用方给的 id 是否已经是一个会话（session-facts.md §6）。
    async fn session_facts(
        &self,
        sessions: &[SessionId],
    ) -> Result<Vec<rust_cli_kernel_api::session_facts::SessionFacts>, StoreError> {
        let sessions = sessions.to_vec();
        self.read(move |rows| rust_cli_node_records::session_facts::lookup(rows, &sessions))
            .await
    }

    /// 工作区的最近列表与它的变化：云端还没有（pg-store.md §14 待确认 1）。
    async fn recent_session_facts(
        &self,
        _workspace: &WorkspaceRef,
        _limit: usize,
    ) -> Result<Vec<rust_cli_kernel_api::session_facts::SessionFacts>, StoreError> {
        Err(unsupported_fact_list())
    }

    /// 分支切点（session-facts.md §2）：与本机同一份读法（node-records 的 `rewind::branch_cut`）。
    async fn branch_cut(&self, session: &SessionId) -> Result<Option<String>, StoreError> {
        let session = session.clone();
        self.read(move |rows| rust_cli_node_records::rewind::branch_cut(rows, &session))
            .await
    }

    /// 库里的子代理（session-lifecycle.md §10.46）：与本机同一份读法（node-records 的 `subagents`）。
    async fn subagents(
        &self,
        session: &SessionId,
    ) -> Result<Vec<rust_cli_kernel_api::agents::StoredSubagent>, StoreError> {
        self.require_live(session).await?;
        let session = session.clone();
        self.read(move |rows| rust_cli_node_records::subagents::stored(rows, &session))
            .await
    }

    /// 历史的一段（node-store.md §5.5、pg-store.md §13）：与本机同一份读法（node-records 的 `load::slice`）。
    async fn history(
        &self,
        session: &SessionId,
        records: &[rust_cli_kernel_api::ids::MessageId],
    ) -> Result<rust_cli_kernel_api::store::HistorySlice, StoreError> {
        let (session, records) = (session.clone(), records.to_vec());
        self.read(move |rows| rust_cli_node_records::load::slice(rows, &session, &records))
            .await
    }

    /// 历史的一段（node-store.md §5.6、pg-store.md §13）：与本机同一份读法（node-records 的 `range::read`）；checkpoint 同 [`Self::load`] 只留本工程写的。
    async fn range(
        &self,
        session: &SessionId,
        span: rust_cli_kernel_api::store::HistorySpan,
    ) -> Result<rust_cli_kernel_api::store::HistoryRange, StoreError> {
        self.require_live(session).await?;
        let owned = session.clone();
        // 云端不记住历史索引（node-store.md §5.7 只在本机）：每次照旧建
        let rust_cli_node_records::range::Read {
            mut range,
            checkpoints,
            ..
        } = self
            .read(move |rows| rust_cli_node_records::range::read(rows, &owned, &span, None))
            .await?;
        let rust = |all: Vec<Active>| {
            all.into_iter()
                .filter_map(|active| match active {
                    Active::Rust(checkpoint) => Some(checkpoint),
                    Active::Node(_) => None,
                })
                .collect()
        };
        range.loaded.checkpoints = rust(checkpoints.active);
        range.loaded.reverted = rust(checkpoints.reverted);
        Ok(range)
    }

    /// 云端不发列表会话事实的变化（pg-store.md §14 待确认 1）：多个进程写同一个库，一个进程里的发布不是变化的唯一来源。
    /// 流不结束也不出项：订阅方看到的是“没有变化”，不是“流断了”。
    fn session_fact_changes(&self) -> rust_cli_kernel_api::session_facts::FactStream {
        use futures_util::StreamExt as _;
        futures_util::stream::pending().boxed()
    }
}

/// 工作区的最近列表会话在云端还没有实现（pg-store.md §14 待确认 1）。
fn unsupported_fact_list() -> StoreError {
    StoreError::Unsupported {
        what: "pg-store recent session list".to_owned(),
    }
}

/// 归档会话的永久删除（node-store.md §5.3）：云端还没有（pg-store.md §14 待确认 2）。Node 的删除凭据表 `session_purge` 与
/// 产物目录都只在本机库。
#[async_trait]
impl rust_cli_kernel_api::deletion::SessionPurge for PgSessionStore {
    async fn deletion_scope(
        &self,
        _root: &SessionId,
    ) -> Result<rust_cli_kernel_api::deletion::DeletionScope, StoreError> {
        Err(unsupported_purge())
    }

    async fn session_files(
        &self,
        _session: &SessionId,
    ) -> Result<Vec<rust_cli_kernel_api::deletion::SessionFile>, StoreError> {
        Err(unsupported_purge())
    }

    async fn begin_deletion(
        &self,
        _root: &SessionId,
        _begin: &rust_cli_kernel_api::deletion::DeletionBegin,
    ) -> Result<rust_cli_kernel_api::deletion::BeginOutcome, StoreError> {
        Err(unsupported_purge())
    }

    async fn remove_session_files(
        &self,
        _session: &SessionId,
        _keep: &[String],
    ) -> Result<(), StoreError> {
        Err(unsupported_purge())
    }

    async fn complete_deletion(&self, _root: &SessionId) -> Result<(), StoreError> {
        Err(unsupported_purge())
    }
}

fn unsupported_purge() -> StoreError {
    StoreError::Unsupported {
        what: "pg-store permanent session deletion".to_owned(),
    }
}
