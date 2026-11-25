//! 内存里的会话存储。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use crate::ids::MessageId;
use crate::ids::SessionId;
use crate::message::AttachmentMessage;
use crate::message::Message;
use crate::session_facts::SessionFacts;
use crate::store::{
    ArtifactPurpose, ArtifactRef, ArtifactStore, CommitReceipt, Fact, FactBatch, ForkReceipt,
    LoadedSession, LocalSettings, SessionFilter, SessionFork, SessionKind, SessionMeta,
    SessionOrder, SessionPage, SessionQuery, SessionRecord, SessionStore, SessionSummary,
    SideChatFork, StoreError, StoreQueries,
};

// 事实的回放（活动历史、checkpoint、标题、回合记录）放在分出的文件里（800 行上限）。
#[path = "store_replay.rs"]
mod replay;
use replay::{entries, first_prompt, last_title, last_turn_failed, replay, split_cold};

/// 内存里的会话存储：记录每次提交的事实，`load` 重放消息、回退游标与 checkpoint。
#[derive(Default)]
pub struct MemoryStore {
    sessions: Mutex<HashMap<SessionId, (SessionMeta, Vec<Fact>)>>,
    sequence: AtomicU64,
    /// 按工作区根记的本机项目规则集（`/permissions`，tui-manage.md §1.1）。
    project_rules: Mutex<HashMap<String, crate::permission::ProjectRules>>,
    /// `activity` 按范围给出的结果（usage.md §5；聚合在 node-store，这里只给前端测试用）。
    activity: Mutex<HashMap<crate::metering::ActivityRange, crate::metering::ActivityStats>>,
    /// 已信任的工作区（workspace-trust.md §2.3）。
    trusted: Mutex<Vec<crate::workspace_trust::TrustKey>>,
    /// 每个批次带的回合锚点，按提交顺序（node-store.md §4，修复线 F6）。
    turns: Mutex<HashMap<SessionId, Vec<Option<crate::store::TurnAnchor>>>>,
    /// 每个批次带的命令 id，按提交顺序（session-lifecycle.md §10.2、§10.3）。
    commands: Mutex<HashMap<SessionId, Vec<Option<crate::ids::CommandId>>>>,
    /// 效果记录（§10.3、§10.30）：带命令 id 的批次里每条 user 一侧消息与这个命令（node-store 把它写在这些消息的 `anchor.sourceCommandId`），
    /// 按提交顺序。
    sources: Mutex<HashMap<SessionId, Vec<crate::store::InputSource>>>,
    /// 软删除的会话（修复线 F29，node-store.md §5.1）：事实保留，读、列、提交都当作不存在。
    deleted: Mutex<HashSet<SessionId>>,
    /// 测试给的活动时间（`set_updated`）；没有时取创建时间。
    updated: Mutex<HashMap<SessionId, u64>>,
    /// 前端记录（缺口 G2）：按写入顺序，同 key 覆盖。
    records: Mutex<HashMap<SessionId, Vec<crate::store::FrontendRecord>>>,
    /// 冷读取按 node-store 的形状切分压缩过的会话（[`Self::split_at_compaction`]，工作线 F102）。
    split: std::sync::atomic::AtomicBool,
    /// 下一个带 `TitleChanged` 的批次提交失败（[`Self::fail_next_title_change`]）。
    fail_title: std::sync::atomic::AtomicBool,
    /// 下一次 `load` 等这道门（[`Self::hold_next_load`]）。
    load_gate: Mutex<Option<std::sync::Arc<tokio::sync::Notify>>>,
    /// 列表会话的事实变化（session-facts.md §3）：写方法改了事实之后发一项（`tracked`）。
    changes: crate::session_facts::FactChanges,
    /// 会话的文件（永久删除的替身，store_purge.rs）：测试经 [`Self::add_file`] 放进去。
    files: Mutex<HashMap<SessionId, Vec<crate::deletion::SessionFile>>>,
    /// 删除凭据（永久删除的替身，store_purge.rs）：根 → 范围、保留的附件与状态。
    receipts: Mutex<HashMap<SessionId, purge::MemoryReceipt>>,
}

impl MemoryStore {
    /// 下一个带 `TitleChanged` 的批次整批提交失败（`Storage` 错误），之后照常（titles.md §4：改名被拒绝时它的记录也不在）。
    pub fn fail_next_title_change(&self) {
        self.fail_title.store(true, Ordering::SeqCst);
    }

    /// 下一次 `load` 等到测试放行（`notify_one`）才读，之后的照常：让测试停在“会话正在打开”的窗口里（titles.md §4）。
    pub fn hold_next_load(&self) -> std::sync::Arc<tokio::sync::Notify> {
        let gate = std::sync::Arc::new(tokio::sync::Notify::new());
        *self.load_gate.lock_unpoisoned() = Some(gate.clone());
        gate
    }

    /// 之后的 `load` 按 node-store 的形状给出压缩过的会话（工作线 F102，compact.md §5.1）：`messages` 从最后一个压缩点开始
    /// （保留段接在摘要之后），之前的放进 `earlier`。缺省不切分（`messages` 是重放出的全部消息）。
    pub fn split_at_compaction(&self) {
        self.split.store(true, Ordering::SeqCst);
    }

    /// 已写下的信任记录（按写入顺序，重复写只留一条）。
    pub fn trusted_keys(&self) -> Vec<crate::workspace_trust::TrustKey> {
        self.trusted.lock_unpoisoned().clone()
    }

    /// 每个成功提交的批次带的回合锚点，按提交顺序。
    pub fn batch_commands(&self, session: &SessionId) -> Vec<Option<crate::ids::CommandId>> {
        self.commands
            .lock_unpoisoned()
            .get(session)
            .cloned()
            .unwrap_or_default()
    }

    pub fn batch_turns(&self, session: &SessionId) -> Vec<Option<crate::store::TurnAnchor>> {
        self.turns
            .lock_unpoisoned()
            .get(session)
            .cloned()
            .unwrap_or_default()
    }

    pub fn facts(&self, session: &SessionId) -> Vec<Fact> {
        self.sessions
            .lock_unpoisoned()
            .get(session)
            .map(|(_, facts)| facts.clone())
            .unwrap_or_default()
    }

    /// 会话的活动时间（`query_sessions` 的排序与显示，测试用）。
    pub fn set_updated(&self, session: &SessionId, at_ms: u64) {
        self.updated
            .lock_unpoisoned()
            .insert(session.clone(), at_ms);
    }

    /// 已删除的会话（修复线 F29）。
    pub fn is_deleted(&self, session: &SessionId) -> bool {
        self.deleted.lock_unpoisoned().contains(session)
    }

    /// 会话现在的标题（最后一次写入；node-store 的会话行）。
    pub fn title(&self, session: &SessionId) -> Option<String> {
        let sessions = self.sessions.lock_unpoisoned();
        let (meta, facts) = sessions.get(session)?;
        Some(last_title(facts).unwrap_or_else(|| meta.title.clone()))
    }

    /// 之后 `activity(range)` 返回的结果。
    pub fn set_activity(
        &self,
        range: crate::metering::ActivityRange,
        stats: crate::metering::ActivityStats,
    ) {
        self.activity.lock_unpoisoned().insert(range, stats);
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn create(
        &self,
        meta: SessionMeta,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        let session = meta.id.clone();
        self.tracked(&session, || {
            self.sessions
                .lock_unpoisoned()
                .insert(session.clone(), (meta, Vec::new()));
            Ok(())
        })?;
        if batch.facts.is_empty() && batch.command.is_none() {
            return Ok(CommitReceipt { sequence: 0 });
        }
        self.commit(&session, batch).await
    }

    async fn commit(
        &self,
        session: &SessionId,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        self.tracked(session, || {
            if self.is_deleted(session) {
                return Err(StoreError::NotFound {
                    session: session.clone(),
                });
            }
            let titled = batch
                .facts
                .iter()
                .any(|fact| matches!(fact, Fact::TitleChanged { .. }));
            if titled && self.fail_title.swap(false, Ordering::SeqCst) {
                return Err(StoreError::Storage {
                    message: "injected title failure".to_owned(),
                });
            }
            let mut sessions = self.sessions.lock_unpoisoned();
            // 草稿落库（session-facts.md §4）：第一项建会话（node-store 的同一规则）；事实里留着它。
            if let Some(Fact::SessionCreated { meta }) = batch.facts.first() {
                sessions.insert(meta.id.clone(), (meta.clone(), Vec::new()));
            }
            let (meta, facts) = sessions
                .get_mut(session)
                .ok_or_else(|| StoreError::NotFound {
                    session: session.clone(),
                })?;
            // 本机的权限事实与 node-store 一样写进会话所在项目的规则集（追加去重、替换、删除、记住的目录）。
            for fact in &batch.facts {
                let mut all = self.project_rules.lock_unpoisoned();
                let project = all
                    .entry(meta.workspace.root.as_str().to_owned())
                    .or_default();
                local_fact(project, fact);
            }
            // node-store 给 user 一侧的消息（人类输入、attachment、meta）写 `anchor.sourceCommandId`，工具结果与压缩摘要不写。
            if let Some(command) = &batch.command {
                let carried = batch.facts.iter().filter_map(|fact| match fact {
                    Fact::MessageAppended {
                        message: Message::User(user),
                    } if !matches!(
                        user.origin,
                        crate::message::UserOrigin::ToolResult { .. }
                            | crate::message::UserOrigin::CompactSummary
                    ) =>
                    {
                        Some(&user.id)
                    }
                    Fact::MessageAppended {
                        message: Message::Attachment(attachment),
                    } => Some(&attachment.id),
                    _ => None,
                });
                self.sources
                    .lock_unpoisoned()
                    .entry(session.clone())
                    .or_default()
                    .extend(carried.map(|message| crate::store::InputSource {
                        message: message.clone(),
                        command: command.clone(),
                    }));
            }
            facts.extend(batch.facts);
            self.commands
                .lock_unpoisoned()
                .entry(session.clone())
                .or_default()
                .push(batch.command);
            self.turns
                .lock_unpoisoned()
                .entry(session.clone())
                .or_default()
                .push(batch.turn);
            Ok(CommitReceipt {
                sequence: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
            })
        })
    }

    async fn load(&self, session: &SessionId) -> Result<LoadedSession, StoreError> {
        let gate = self.load_gate.lock_unpoisoned().take();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        if self.is_deleted(session) {
            return Err(StoreError::NotFound {
                session: session.clone(),
            });
        }
        let sessions = self.sessions.lock_unpoisoned();
        let (meta, facts) = sessions.get(session).ok_or_else(|| StoreError::NotFound {
            session: session.clone(),
        })?;
        let (messages, checkpoints, reverted) = replay(facts);
        let sources = self
            .sources
            .lock_unpoisoned()
            .get(session)
            .cloned()
            .unwrap_or_default();
        // 标题与 node-store 的会话行一样取最后一次写入（会话关闭后再打开时 actor 从这里拿标题）。
        let title = last_title(facts);
        let mut meta = meta.clone();
        if let Some(title) = title {
            meta.title = title;
        }
        let (earlier, messages) = if self.split.load(Ordering::SeqCst) {
            split_cold(messages)
        } else {
            (Vec::new(), messages)
        };
        Ok(LoadedSession {
            meta,
            messages,
            earlier,
            model: None,
            mode: None,
            plan: false,
            checkpoints,
            reverted,
            // 最后一个批次之后的当前轮（node-store 按库推导；这里取最后一个带锚点的批次）。
            turn: self
                .turns
                .lock_unpoisoned()
                .get(session)
                .and_then(|turns| turns.iter().rev().find_map(Clone::clone)),
            last_turn_failed: last_turn_failed(facts),
            turns: entries(facts, crate::store::entry_kinds::TURN)
                .into_iter()
                .filter_map(|(id, data)| crate::store::TurnRecord::from_entry(&id, &data))
                .collect(),
            inputs: entries(facts, crate::store::entry_kinds::INPUT)
                .into_iter()
                .filter_map(|(id, data)| crate::input_extras::InputRecord::from_entry(&id, &data))
                .collect(),
            plans: entries(facts, crate::store::entry_kinds::PLAN)
                .into_iter()
                .filter_map(|(id, data)| crate::plan::ShownPlan::from_entry(&id, &data))
                .collect(),
            sources,
            // 输出头部（session-lifecycle.md §10.34）：node-store 写进 tool part、冷读读回；这里按提交顺序取事实。
            output_heads: facts
                .iter()
                .filter_map(|fact| match fact {
                    Fact::OutputHead { head } => Some(head.clone()),
                    _ => None,
                })
                .collect(),
            // 工具调用的开工与结束（§10.37）：node-store 按同批的计量写进 tool part；这里按提交顺序取计量。
            tool_times: facts
                .iter()
                .filter_map(|fact| match fact {
                    Fact::ToolCalled { call } => Some(call.time()),
                    _ => None,
                })
                .collect(),
        })
    }

    async fn list(&self, _filter: SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
        let deleted = self.deleted.lock_unpoisoned().clone();
        Ok(self
            .sessions
            .lock_unpoisoned()
            .values()
            .filter(|(meta, _)| !deleted.contains(&meta.id))
            .map(|(meta, _)| SessionSummary {
                id: meta.id.clone(),
                title: meta.title.clone(),
                updated_at_ms: meta.created_at_ms,
            })
            .collect())
    }

    /// `Fact::Entry` 按写入顺序（node-store 的同一口径，cross-session.md §2.2）。
    async fn entries(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<(String, serde_json::Value)>, StoreError> {
        let sessions = self.sessions.lock_unpoisoned();
        let (_, facts) = sessions.get(session).ok_or_else(|| StoreError::NotFound {
            session: session.clone(),
        })?;
        Ok(entries(facts, kind))
    }

    /// Node `/fork` 的替身：活动历史到 `through` 为止原样复制（内存里不必换 ID），末尾是一条
    /// `source = fork` 的 attachment（正文是简化的分叉提示）。
    async fn fork(&self, fork: SessionFork) -> Result<ForkReceipt, StoreError> {
        self.tracked(&fork.child.clone(), || {
            let mut sessions = self.sessions.lock_unpoisoned();
            // 同 node-store：`child` 已经是这个父会话分出的 fork → 什么都不写；是别的会话 → 拒绝（session-lifecycle.md §10.57）
            if let Some((child, _)) = sessions.get(&fork.child) {
                return if child.kind == SessionKind::Fork
                    && child.parent.as_ref() == Some(&fork.parent)
                {
                    Ok(ForkReceipt::Existing)
                } else {
                    Err(StoreError::Exists {
                        session: fork.child.clone(),
                    })
                };
            }
            let (parent, facts) =
                sessions
                    .get(&fork.parent)
                    .cloned()
                    .ok_or_else(|| StoreError::NotFound {
                        session: fork.parent.clone(),
                    })?;
            let (messages, _, _) = replay(&facts);
            let end = messages
                .iter()
                .position(|m| m.id() == &fork.through)
                .ok_or_else(|| StoreError::Storage {
                    message: format!(
                        "Checkpoint message not found in session store: {}",
                        fork.through
                    ),
                })?
                + 1;
            let title = if parent.title.is_empty() {
                crate::title::UNTITLED
            } else {
                parent.title.as_str()
            };
            let meta = SessionMeta {
                id: fork.child.clone(),
                kind: SessionKind::Fork,
                parent: Some(parent.id.clone()),
                workspace: parent.workspace.clone(),
                title: format!("Fork of {title}"),
                trace: parent.trace.clone(),
                created_at_ms: fork.created_at_ms,
            };
            let notice = Message::Attachment(AttachmentMessage {
                id: MessageId::new(format!("msg_fork_notice_{}", fork.child)),
                source: "fork".to_owned(),
                text: rust_cli_base::reminder::wrap(&format!(
                    "This session was forked from {}.",
                    parent.id
                )),
                data: serde_json::Value::Null,
            });
            let mut copied = vec![
                Fact::ModelSelected {
                    selection: fork.selection,
                },
                Fact::ExecutionChanged {
                    state: crate::permission::ExecutionState::from_parts(fork.mode, fork.plan),
                },
            ];
            copied.extend(
                messages[..end]
                    .iter()
                    .chain(std::iter::once(&notice))
                    .map(|message| Fact::MessageAppended {
                        message: message.clone(),
                    }),
            );
            copied.extend(fork_records::copied_records(&facts, &messages[..end]));
            sessions.insert(fork.child, (meta, copied));
            Ok(ForkReceipt::Created { copied: end })
        })
    }

    async fn side_chat(&self, fork: SideChatFork) -> Result<ForkReceipt, StoreError> {
        self.side_chat_fork(fork)
    }

    /// node-store 的同一口径（node-store.md §5.1）：按根目录筛、删除的不列、按时间从新到旧（同一时刻 id 大的在前）。
    async fn query_sessions(&self, query: SessionQuery) -> Result<SessionPage, StoreError> {
        let deleted = self.deleted.lock_unpoisoned().clone();
        let updated = self.updated.lock_unpoisoned().clone();
        let mut records: Vec<SessionRecord> = self
            .sessions
            .lock_unpoisoned()
            .values()
            .filter(|(meta, _)| !deleted.contains(&meta.id) && listed(meta.kind))
            .filter(|(meta, _)| {
                query
                    .root
                    .as_ref()
                    .is_none_or(|root| &meta.workspace.root == root)
            })
            .map(|(meta, facts)| SessionRecord {
                id: meta.id.clone(),
                title: last_title(facts).unwrap_or_else(|| meta.title.clone()),
                root: meta.workspace.root.clone(),
                created_at_ms: meta.created_at_ms,
                updated_at_ms: updated.get(&meta.id).copied().unwrap_or(meta.created_at_ms),
                first_prompt: first_prompt(facts),
            })
            .collect();
        records.sort_by(|a, b| {
            let key = |r: &SessionRecord| match query.order {
                SessionOrder::Updated => r.updated_at_ms,
                SessionOrder::Created => r.created_at_ms,
            };
            key(b)
                .cmp(&key(a))
                .then_with(|| b.id.as_str().cmp(a.id.as_str()))
        });
        let more = query.limit > 0 && records.len() > query.limit;
        if query.limit > 0 {
            records.truncate(query.limit);
        }
        Ok(SessionPage {
            sessions: records,
            more,
        })
    }

    async fn rename(&self, session: &SessionId, title: &str) -> Result<(), StoreError> {
        self.tracked(session, || {
            if self.is_deleted(session) {
                return Err(StoreError::NotFound {
                    session: session.clone(),
                });
            }
            let mut sessions = self.sessions.lock_unpoisoned();
            let (_, facts) = sessions
                .get_mut(session)
                .ok_or_else(|| StoreError::NotFound {
                    session: session.clone(),
                })?;
            facts.push(Fact::TitleChanged {
                title: title.to_owned(),
            });
            Ok(())
        })
    }

    async fn delete(&self, session: &SessionId) -> Result<(), StoreError> {
        self.tracked(session, || {
            if !self.sessions.lock_unpoisoned().contains_key(session) {
                return Err(StoreError::NotFound {
                    session: session.clone(),
                });
            }
            self.deleted.lock_unpoisoned().insert(session.clone());
            Ok(())
        })
    }
}

#[async_trait]
impl LocalSettings for MemoryStore {
    async fn project_rules(
        &self,
        workspace: &crate::env::WorkspaceRef,
    ) -> Result<crate::permission::ProjectRules, StoreError> {
        Ok(self
            .project_rules
            .lock_unpoisoned()
            .get(workspace.root.as_str())
            .cloned()
            .unwrap_or_default())
    }

    /// 与 node-store 相同的语义：追加去重。
    async fn remember_directories(
        &self,
        workspace: &crate::env::WorkspaceRef,
        directories: Vec<crate::env::EnvPath>,
    ) -> Result<(), StoreError> {
        let mut all = self.project_rules.lock_unpoisoned();
        let rules = all.entry(workspace.root.as_str().to_owned()).or_default();
        local_fact(rules, &Fact::DirectoriesRemembered { directories });
        Ok(())
    }

    /// 与 node-store 相同的语义：这一类里先删、再加并去重（保留先出现的）。
    async fn change_project_rules(
        &self,
        workspace: &crate::env::WorkspaceRef,
        change: crate::store::ProjectRulesChange,
    ) -> Result<crate::permission::ProjectRules, StoreError> {
        use crate::permission::RuleBehavior;
        let mut all = self.project_rules.lock_unpoisoned();
        let rules = all.entry(workspace.root.as_str().to_owned()).or_default();
        let list = match change.behavior {
            RuleBehavior::Allow => &mut rules.allow,
            RuleBehavior::Deny => &mut rules.deny,
            RuleBehavior::Ask => &mut rules.ask,
        };
        list.retain(|rule| !change.remove.contains(rule));
        for rule in change.add {
            if !list.contains(&rule) {
                list.push(rule);
            }
        }
        Ok(rules.clone())
    }

    async fn trusted_workspace(
        &self,
        candidates: &[crate::workspace_trust::TrustKey],
    ) -> Result<Option<crate::workspace_trust::TrustKey>, StoreError> {
        let trusted = self.trusted.lock_unpoisoned();
        Ok(candidates.iter().find(|key| trusted.contains(key)).cloned())
    }

    async fn trust_workspace(
        &self,
        key: &crate::workspace_trust::TrustKey,
    ) -> Result<(), StoreError> {
        let mut trusted = self.trusted.lock_unpoisoned();
        if !trusted.contains(key) {
            trusted.push(key.clone());
        }
        Ok(())
    }

    /// 群协作会话自己的规则集：内存里没有这种会话（node-store 读会话行的 `permission.scope`）。
    async fn session_rules(
        &self,
        _session: &SessionId,
    ) -> Result<Option<crate::permission::ProjectRules>, StoreError> {
        Ok(None)
    }

    /// 本机项目的持久模式：内存里不记（node-store 读 `local_setting`），总是没有。
    async fn project_mode(
        &self,
        _workspace: &crate::env::WorkspaceRef,
    ) -> Result<Option<crate::permission::PermissionMode>, StoreError> {
        Ok(None)
    }
}

#[async_trait]
impl StoreQueries for MemoryStore {
    /// 会话与以它为祖先的会话里 `ModelCalled{status: completed}` 的耗时之和（node-store 的同一口径，usage.md §4）。
    async fn metering(
        &self,
        session: &SessionId,
    ) -> Result<crate::metering::SessionMetering, StoreError> {
        let sessions = self.sessions.lock_unpoisoned();
        if !sessions.contains_key(session) {
            return Err(StoreError::NotFound {
                session: session.clone(),
            });
        }
        let descends = |start: &SessionId| {
            let mut id = start.clone();
            loop {
                if &id == session {
                    return true;
                }
                match sessions.get(&id).and_then(|(meta, _)| meta.parent.clone()) {
                    Some(parent) => id = parent,
                    None => return false,
                }
            }
        };
        let api_duration_ms = sessions
            .iter()
            .filter(|(id, _)| descends(id))
            .flat_map(|(_, (_, facts))| facts)
            .filter_map(|fact| match fact {
                Fact::ModelCalled { call }
                    if call.status == crate::metering::CallStatus::Completed =>
                {
                    Some(call.completed_at_ms.saturating_sub(call.started_at_ms))
                }
                _ => None,
            })
            .sum();
        Ok(crate::metering::SessionMetering { api_duration_ms })
    }

    /// 历史的一段（node-store.md §5.5，node-store 的同一口径）：重放出的消息里 [`Message::record`] 在 `records` 之中的，按 `records` 的顺序
    /// （同一条存储的消息里的先后不变）；回合记录取 `first` 在其中的，输出头部与起止取这些消息里的调用的。
    async fn history(
        &self,
        session: &SessionId,
        records: &[crate::ids::MessageId],
    ) -> Result<crate::store::HistorySlice, StoreError> {
        let sessions = self.sessions.lock_unpoisoned();
        let (_, facts) = sessions.get(session).ok_or_else(|| StoreError::NotFound {
            session: session.clone(),
        })?;
        let (messages, _, _) = replay(facts);
        let mut messages: Vec<Message> = records
            .iter()
            .flat_map(|record| {
                messages
                    .iter()
                    .filter(move |message| message.record() == record)
            })
            .cloned()
            .collect();
        messages.dedup_by(|a, b| a.id() == b.id());
        let calls: std::collections::HashSet<&crate::ids::ToolUseId> =
            messages.iter().flat_map(Message::tool_uses).collect();
        let mut slice = crate::store::HistorySlice {
            turns: entries(facts, crate::store::entry_kinds::TURN)
                .into_iter()
                .filter_map(|(id, data)| crate::store::TurnRecord::from_entry(&id, &data))
                .filter(|turn| records.contains(&turn.first))
                .collect(),
            ..Default::default()
        };
        for fact in facts {
            match fact {
                Fact::OutputHead { head } if calls.contains(&head.tool_use) => {
                    slice.output_heads.push(head.clone());
                }
                Fact::ToolCalled { call } if calls.contains(&call.time().tool_use) => {
                    slice.tool_times.push(call.time());
                }
                _ => {}
            }
        }
        slice.messages = messages;
        Ok(slice)
    }

    /// 历史的一段（node-store.md §5.6，node-store 的同一口径）：整段读出再按显示顺序选段（`store_range`）。
    async fn range(
        &self,
        session: &SessionId,
        span: crate::store::HistorySpan,
    ) -> Result<crate::store::HistoryRange, StoreError> {
        let loaded = SessionStore::load(self, session).await?;
        let subagents = crate::store::StoreQueries::subagents(self, session).await?;
        store_range::range(loaded, subagents, &span)
    }

    /// 会话里的 `SubagentLinked`（node-store 的同一口径，session-lifecycle.md §10.46；`store_subagents`）。
    async fn subagents(
        &self,
        session: &SessionId,
    ) -> Result<Vec<crate::agents::StoredSubagent>, StoreError> {
        let sessions = self.sessions.lock_unpoisoned();
        store_subagents::subagents(&sessions, session)
    }

    /// 会话里 `ModelCalled{source: 主线程或子代理, status: completed}` 的聚合（node-store 的同一口径，usage.md §4.1；
    /// 子代理的请求只在子会话里，F112）：用量缺失时输入与缓存读都算未知；“最近一个”是最后提交的那个。
    async fn request_stats(
        &self,
        session: &SessionId,
    ) -> Result<crate::metering::RequestStats, StoreError> {
        let sessions = self.sessions.lock_unpoisoned();
        let (_, facts) = sessions.get(session).ok_or_else(|| StoreError::NotFound {
            session: session.clone(),
        })?;
        let mut stats = crate::metering::RequestStats::default();
        for fact in facts {
            let Fact::ModelCalled { call } = fact else {
                continue;
            };
            let own_loop = matches!(
                call.source,
                crate::model::QuerySource::MainThread | crate::model::QuerySource::Subagent(_)
            );
            if !own_loop || call.status != crate::metering::CallStatus::Completed {
                continue;
            }
            stats.requests += 1;
            match &call.usage {
                // Node 归一化的输入含缓存读写（usage.md §2.2）。
                Some(usage) => {
                    stats.input_tokens += usage.input_tokens
                        + usage.cache_creation_input_tokens
                        + usage.cache_read_input_tokens;
                    stats.cache_read_tokens += usage.cache_read_input_tokens;
                }
                None => stats.unknown_cache_usage = true,
            }
            let output = call.usage.map(|usage| usage.output_tokens);
            stats.output_tokens += output.unwrap_or(0);
            let generation = call.first_token_at_ms.and_then(|first| {
                let ms = call.completed_at_ms.saturating_sub(first);
                (ms > 0).then_some(ms)
            });
            if let (Some(output), Some(ms)) = (output.filter(|o| *o > 0), generation) {
                stats.measured_output_tokens += output;
                stats.generation_ms += ms;
            }
            stats.last = Some(crate::metering::LastRequest {
                output_tokens: output,
                duration_ms: Some(call.completed_at_ms.saturating_sub(call.started_at_ms)),
                first_token_ms: call
                    .first_token_at_ms
                    .map(|first| first.saturating_sub(call.started_at_ms)),
            });
        }
        Ok(stats)
    }

    async fn activity(
        &self,
        query: crate::metering::ActivityQuery,
    ) -> Result<crate::metering::ActivityStats, StoreError> {
        Ok(self
            .activity
            .lock_unpoisoned()
            .get(&query.range)
            .cloned()
            .unwrap_or_else(|| crate::metering::ActivityStats {
                range: Some(query.range),
                today: query.today,
                ..Default::default()
            }))
    }

    /// 前端记录（缺口 G2）：会话要存在；同 key 覆盖原位置。
    async fn put_record(
        &self,
        session: &SessionId,
        record: &crate::store::FrontendRecord,
    ) -> Result<(), StoreError> {
        if !self.sessions.lock_unpoisoned().contains_key(session) {
            return Err(StoreError::NotFound {
                session: session.clone(),
            });
        }
        let mut records = self.records.lock_unpoisoned();
        let list = records.entry(session.clone()).or_default();
        match list.iter_mut().find(|known| known.key == record.key) {
            Some(known) => *known = record.clone(),
            None => list.push(record.clone()),
        }
        Ok(())
    }

    async fn records(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<crate::store::FrontendRecord>, StoreError> {
        Ok(self
            .records
            .lock_unpoisoned()
            .get(session)
            .map(|list| {
                list.iter()
                    .filter(|record| record.kind == kind)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn session_facts(&self, sessions: &[SessionId]) -> Result<Vec<SessionFacts>, StoreError> {
        Ok(sessions.iter().filter_map(|id| self.facts_of(id)).collect())
    }

    /// 最后一个 `RevertSet` 的游标（node-store 是会话行的 `revert` 原文；这里只要「变了就不同」）。
    async fn branch_cut(&self, session: &SessionId) -> Result<Option<String>, StoreError> {
        let sessions = self.sessions.lock_unpoisoned();
        let (_, facts) = sessions.get(session).ok_or_else(|| StoreError::NotFound {
            session: session.clone(),
        })?;
        Ok(facts.iter().rev().find_map(|fact| match fact {
            Fact::RevertSet { cursor } => Some(serde_json::json!(cursor).to_string()),
            _ => None,
        }))
    }

    async fn recent_session_facts(
        &self,
        workspace: &crate::env::WorkspaceRef,
        limit: usize,
    ) -> Result<Vec<SessionFacts>, StoreError> {
        Ok(self.recent_listed(workspace, limit))
    }

    fn session_fact_changes(&self) -> crate::session_facts::FactStream {
        self.changes.subscribe()
    }
}

// 会话事实的替身（session-facts.md §2）放在分出的文件里（800 行上限）。
#[path = "store_rules.rs"]
mod rules;

#[path = "store_subagents.rs"]
mod store_subagents;
use rules::local_fact;

#[path = "store_facts.rs"]
mod facts;
use facts::listed;

#[path = "store_fork.rs"]
mod fork_records;

#[path = "store_side_chat.rs"]
mod side_chat;

#[path = "store_purge.rs"]
mod purge;

#[path = "store_range.rs"]
mod store_range;

#[async_trait]
impl ArtifactStore for MemoryStore {
    async fn put(
        &self,
        _session: &SessionId,
        _purpose: ArtifactPurpose,
        _media_type: String,
        _bytes: Vec<u8>,
    ) -> Result<ArtifactRef, StoreError> {
        Err(StoreError::Unsupported {
            what: "memory artifacts".to_owned(),
        })
    }

    async fn get(&self, _artifact: &ArtifactRef) -> Result<Vec<u8>, StoreError> {
        Err(StoreError::Unsupported {
            what: "memory artifacts".to_owned(),
        })
    }
}
