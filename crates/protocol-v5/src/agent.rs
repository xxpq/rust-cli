//! 进程级 `agent` 与每个工作区的 `ws/<key>/agent`（01 §17.1，修订 1；protocol-v5.md §6）。
//!
//! ```text
//! 进程级 phase：starting ──存储失败──► degraded
//!                  └──进程级部件建好（Ready）──► ready
//! 工作区 phase（rust 合成）：进程没就绪 = 进程的 phase；进程就绪后 = 这个工作区内核的状态
//!   Waiting（等进程）──Ready──► Opening(starting) ──组装完──► Open(ready) / Failed(degraded)
//!   Open ──agent.release──► Stopped(stopped) ──再订阅 / 再用──► Opening
//! ```
//!
//! 组装在别的任务里做（`Workspaces::open`），完成时发 `Opened` 回 actor；`generation` 区分过期的完成（释放之后才到的）。

use std::collections::BTreeMap;
use std::sync::Arc;

use rust_cli_kernel_api::store::StoreQueries;
use serde_json::{Value, json};

use crate::consts::IMPLEMENTATION;
use crate::generated::agent_v1::{
    AgentStateDiagnostics, AgentStatePhase, AgentStateStorage, AgentStateStoragePhase,
};
use crate::generated::contract::CONTRACT_DIGEST;
use crate::publisher::Publisher;
use crate::server::{Actor, Event, Storage, Workspaces};
use crate::topics::{AGENT_TOPIC, WORKSPACE_AGENT, workspace_topic};

/// 进程级的状态。
pub(crate) struct Process {
    pub(crate) phase: AgentStatePhase,
    /// 进程级部件建好之后才有：组装工作区用。
    pub(crate) workspaces: Option<Arc<dyn Workspaces>>,
    /// 进程级部件建好之后才有（与 `workspaces` 同时）：会话列表的事实与变化（protocol-v5.md §14）。
    pub(crate) queries: Option<Arc<dyn StoreQueries>>,
    database: Option<String>,
}

impl Process {
    pub(crate) fn new() -> Self {
        Self {
            phase: AgentStatePhase::Starting,
            workspaces: None,
            queries: None,
            database: None,
        }
    }

    fn ready(&self) -> bool {
        self.phase == AgentStatePhase::Ready
    }
}

/// 一个工作区的内核在本进程里的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceState {
    /// 等进程级部件建好。
    Waiting,
    Opening,
    Open,
    Failed(String),
    /// `agent.release` 关掉了。
    Stopped,
}

pub(crate) struct Workspace {
    pub(crate) state: WorkspaceState,
    /// 每次开始组装或释放加一：只认最新一次组装的完成。
    pub(crate) generation: u64,
}

fn patch(items: &[(&str, Value)]) -> BTreeMap<String, Value> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn phase_value(phase: &AgentStatePhase) -> Value {
    json!(phase.as_str())
}

impl Actor {
    /// 进程级 `agent` 在 actor 建好时登记：starting、build、诊断字段（01 §17.4：只进日志与诊断）。
    pub(crate) fn register_process_topic(&mut self) {
        let mut publisher = Publisher::new(AGENT_TOPIC, self.epoch.clone());
        let diagnostics = AgentStateDiagnostics {
            implementation: IMPLEMENTATION.to_owned(),
            version: self.settings.version.clone(),
            protocol_digest: Some(CONTRACT_DIGEST.to_owned()),
        };
        publisher.set_state(patch(&[
            ("phase", phase_value(&self.process.phase)),
            ("build", json!(self.settings.version)),
            ("hostServices", json!([])),
            ("providers", json!([])),
            (
                "diagnostics",
                serde_json::to_value(diagnostics).unwrap_or(Value::Null),
            ),
        ]));
        self.topics.insert(AGENT_TOPIC.to_owned(), publisher);
    }

    /// 握手里 Host 告知的服务（protocol-v5.md §30）：记下（发 `service` 请求前查它），进程级 `agent.hostServices` 写同一个值。
    pub(crate) fn declare_host_services(&mut self, services: Vec<String>) {
        self.publish(AGENT_TOPIC, &[("hostServices", json!(services))]);
        self.host_services = services;
    }

    fn publish(&mut self, topic: &str, items: &[(&str, Value)]) {
        if let Some(publisher) = self.topics.get_mut(topic)
            && publisher.set_state(patch(items))
        {
            self.outbox.mark_topic(topic);
        }
    }

    fn storage_state(&self, phase: AgentStateStoragePhase, error: Option<String>) -> Value {
        let storage = AgentStateStorage {
            database_id: self.process.database.clone().unwrap_or_default(),
            phase,
            migration: None,
            error_code: error,
            sqlite_code: None,
            system_code: None,
            migration_id: None,
        };
        serde_json::to_value(storage).unwrap_or(Value::Null)
    }

    pub(crate) fn storage(&mut self, storage: Storage) {
        match storage {
            Storage::Checking { database_id } => {
                self.process.database = Some(database_id);
                let state = self.storage_state(AgentStateStoragePhase::Checking, None);
                self.publish(AGENT_TOPIC, &[("storage", state)]);
            }
            Storage::Ready => {
                let state = self.storage_state(AgentStateStoragePhase::Ready, None);
                self.publish(AGENT_TOPIC, &[("storage", state)]);
            }
            Storage::Failed { code } => {
                let state = self.storage_state(AgentStateStoragePhase::Failed, Some(code));
                self.process.phase = AgentStatePhase::Degraded;
                let phase = phase_value(&self.process.phase);
                self.publish(AGENT_TOPIC, &[("storage", state), ("phase", phase)]);
                self.republish_workspaces();
            }
        }
    }

    /// 进程级部件建好：进程 ready；等着的工作区开始组装（等着的会话列表由 `materialize_waiting` 物化）。
    pub(crate) fn process_ready(
        &mut self,
        workspaces: Arc<dyn Workspaces>,
        queries: Arc<dyn StoreQueries>,
    ) {
        self.process.workspaces = Some(workspaces);
        self.process.queries = Some(queries);
        self.process.phase = AgentStatePhase::Ready;
        let phase = phase_value(&self.process.phase);
        self.publish(AGENT_TOPIC, &[("phase", phase)]);
        let waiting: Vec<String> = self
            .workspaces
            .iter()
            .filter(|(_, w)| w.state == WorkspaceState::Waiting)
            .map(|(key, _)| key.clone())
            .collect();
        for key in waiting {
            self.open(&key);
        }
    }

    /// 工作区的 phase（rust 合成，01 §17.1）。
    fn workspace_phase(&self, state: &WorkspaceState) -> AgentStatePhase {
        match state {
            WorkspaceState::Waiting => self.process.phase.clone(),
            WorkspaceState::Opening => AgentStatePhase::Starting,
            WorkspaceState::Open => AgentStatePhase::Ready,
            WorkspaceState::Failed(_) => AgentStatePhase::Degraded,
            WorkspaceState::Stopped => AgentStatePhase::Stopped,
        }
    }

    fn set_workspace(&mut self, key: &str, state: WorkspaceState) {
        let phase = self.workspace_phase(&state);
        if let Some(workspace) = self.workspaces.get_mut(key) {
            workspace.state = state;
        }
        let topic = workspace_topic(key, WORKSPACE_AGENT);
        self.publish(&topic, &[("phase", phase_value(&phase))]);
    }

    fn republish_workspaces(&mut self) {
        let keys: Vec<(String, WorkspaceState)> = self
            .workspaces
            .iter()
            .map(|(k, w)| (k.clone(), w.state.clone()))
            .collect();
        for (key, state) in keys {
            self.set_workspace(&key, state);
        }
    }

    /// 订阅 `ws/<key>/agent` 时：没有就登记（订阅即物化）；释放过的重新组装（“之后第一次订阅按需重新组装”）。
    pub(crate) fn ensure_workspace(&mut self, key: &str) {
        let topic = workspace_topic(key, WORKSPACE_AGENT);
        if !self.topics.contains_key(&topic) {
            let mut publisher = Publisher::new(topic.clone(), self.epoch.clone());
            let phase = self.workspace_phase(&WorkspaceState::Waiting);
            publisher.set_state(patch(&[("phase", phase_value(&phase))]));
            self.topics.insert(topic, publisher);
            self.workspaces.insert(
                key.to_owned(),
                Workspace {
                    state: WorkspaceState::Waiting,
                    generation: 0,
                },
            );
            if self.process.ready() {
                self.open(key);
            }
            return;
        }
        let stopped = self
            .workspaces
            .get(key)
            .is_some_and(|w| w.state == WorkspaceState::Stopped);
        if stopped && self.process.ready() {
            self.open(key);
        }
    }

    /// 开始组装一个工作区（不等）：完成时发 `Opened` 回来。
    fn open(&mut self, key: &str) {
        let Some(workspaces) = self.process.workspaces.clone() else {
            return;
        };
        let generation = match self.workspaces.get_mut(key) {
            Some(workspace) => {
                workspace.generation += 1;
                workspace.generation
            }
            None => return,
        };
        self.set_workspace(key, WorkspaceState::Opening);
        let events = self.events.clone();
        let key = key.to_owned();
        drop(self.spawner.spawn("protocol-v5-workspace", async move {
            let result = workspaces.open(&key).await.map(|_| ());
            let _ = events.send(Event::Opened {
                key,
                generation,
                result,
            });
        }));
    }

    pub(crate) fn opened(&mut self, key: &str, generation: u64, result: Result<(), String>) {
        let current = self
            .workspaces
            .get(key)
            .is_some_and(|w| w.generation == generation && w.state == WorkspaceState::Opening);
        if !current {
            // 释放之后才到的完成：丢掉（那次组装的内核已经不在缓存里）
            return;
        }
        match result {
            Ok(()) => self.set_workspace(key, WorkspaceState::Open),
            Err(reason) => {
                tracing::warn!(event = "protocol.workspace.failed", workspace = key, error = %reason, "assembling a workspace failed");
                self.set_workspace(key, WorkspaceState::Failed(reason));
            }
        }
    }

    /// 释放一个工作区（`agent.release`）：打开着（或正在组装）时关掉，返回真；否则什么也不做。
    pub(crate) fn release_workspace(&mut self, key: &str) -> bool {
        let live = self
            .workspaces
            .get(key)
            .is_some_and(|w| matches!(w.state, WorkspaceState::Open | WorkspaceState::Opening));
        if !live {
            return false;
        }
        if let Some(workspaces) = &self.process.workspaces {
            workspaces.release(key);
        }
        if let Some(workspace) = self.workspaces.get_mut(key) {
            workspace.generation += 1;
        }
        self.set_workspace(key, WorkspaceState::Stopped);
        true
    }
}
