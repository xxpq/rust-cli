//! 任务清单 Task v2（docs/specs/rust-cli/agents.md §7）。
//!
//! 清单属于顶层会话；文件（[`TaskListStore`]）是唯一事实源。读改写都交给 [`TaskBoard`] 的 owner 任务
//! 按顺序处理（本进程是清单唯一的写者），写成功后由它经信号把新清单交给顶层会话的 actor（`TasksChanged`）。

mod edit;
mod tools;

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rust_cli_base::Spawner;
use tokio::sync::{mpsc, oneshot};

use async_trait::async_trait;
use rust_cli_kernel_api::agents::{TaskList, TaskListStore, TaskRecord};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::StoreError;
use serde_json::json;
pub(crate) use tools::tools;

use crate::signals::Hub;

/// 模型没有卡片的内核行为，或显式打开时启用（agents.md §7.1）。有卡片的模型与按 opus 处理的 GLM 默认关闭。
///
/// 修改（工作线 F121）：以前另有一份 15 个旧模型的清单，那些模型都没有卡片，“没有卡片就打开”已经覆盖，删掉。
pub fn task_tools_enabled(model: &str, opt_in: bool) -> bool {
    opt_in || rust_cli_prompts::models::behavior(model).is_none()
}

/// 没有本地存储时（测试、嵌入）用的内存清单。
#[derive(Default)]
pub struct MemoryTaskLists(Mutex<HashMap<SessionId, TaskList>>);

#[async_trait]
impl TaskListStore for MemoryTaskLists {
    async fn load(&self, session: &SessionId) -> Result<TaskList, StoreError> {
        let lists = self.0.lock_unpoisoned();
        Ok(lists.get(session).cloned().unwrap_or_default())
    }

    async fn save(&self, session: &SessionId, list: &TaskList) -> Result<(), StoreError> {
        let mut lists = self.0.lock_unpoisoned();
        lists.insert(session.clone(), list.clone());
        Ok(())
    }
}

/// 一次修改：返回给模型的文本与“是否改了”。
pub(crate) type Change = Box<dyn FnOnce(&mut TaskList) -> (String, bool) + Send>;

enum Job {
    Read {
        list: SessionId,
        reply: oneshot::Sender<Result<TaskList, StoreError>>,
    },
    Update {
        list: SessionId,
        change: Change,
        reply: oneshot::Sender<Result<String, StoreError>>,
    },
}

/// 任务清单的 owner：一个任务按顺序处理全部读改写，同一份清单不会交错修改（不靠跨 await 的锁）。
pub(crate) struct TaskBoard {
    jobs: mpsc::Sender<Job>,
}

fn closed() -> StoreError {
    StoreError::Storage {
        message: "the task board is closed".to_owned(),
    }
}

/// 一次读改写：改了才保存，保存成功后宣布新清单。
///
/// 修 bug（agents.md §7.2，SMALL-6）：以前由调用的工具在拿到回复之后宣布。两个写者（主线程与后台子代理）交错时，先写完的那个
/// 可能后宣布，`Hub` 只留最后到的一份，会话最后收到的是旧清单。owner 在处理下一件事之前宣布，宣布的顺序就是写入的顺序。
async fn update(
    store: &dyn TaskListStore,
    hub: &Hub,
    list: &SessionId,
    change: Change,
) -> Result<String, StoreError> {
    let mut tasks = store.load(list).await?;
    let (text, changed) = change(&mut tasks);
    if changed {
        store.save(list, &tasks).await?;
        hub.tasks_changed(list, json!(tasks.tasks));
    }
    Ok(text)
}

async fn serve(store: Arc<dyn TaskListStore>, hub: Hub, mut jobs: mpsc::Receiver<Job>) {
    while let Some(job) = jobs.recv().await {
        match job {
            Job::Read { list, reply } => {
                let _ = reply.send(store.load(&list).await);
            }
            Job::Update {
                list,
                change,
                reply,
            } => {
                let _ = reply.send(update(store.as_ref(), &hub, &list, change).await);
            }
        }
    }
}

/// 任务清单写者的排队容量。
const JOB_QUEUE: usize = 64;

impl TaskBoard {
    /// 写成功的清单经 `hub` 交给顶层会话（`TasksChanged`）。
    pub(crate) fn new(store: Arc<dyn TaskListStore>, hub: Hub, spawner: &Spawner) -> Self {
        let (jobs, receiver) = mpsc::channel(JOB_QUEUE);
        let _task = spawner.spawn_resident("task-board", serve(store, hub, receiver));
        Self { jobs }
    }

    pub(crate) async fn read(&self, list: &SessionId) -> Result<TaskList, StoreError> {
        let (reply, answer) = oneshot::channel();
        let job = Job::Read {
            list: list.clone(),
            reply,
        };
        self.jobs.send(job).await.map_err(|_| closed())?;
        answer.await.map_err(|_| closed())?
    }

    /// 清单里的任务（会话的视图、冷快照与任务提醒）：读不出时记一条 warn、给空清单，之后的 `TasksChanged` 照常更新。
    pub(crate) async fn tasks(&self, list: &SessionId) -> Vec<TaskRecord> {
        match self.read(list).await {
            Ok(list) => list.tasks,
            Err(error) => {
                tracing::warn!(%error, "the task list could not be read");
                Vec::new()
            }
        }
    }

    /// 读、改、写；没改时不写文件。返回给模型的文本。
    pub(crate) async fn update(
        &self,
        list: &SessionId,
        change: Change,
    ) -> Result<String, StoreError> {
        let (reply, answer) = oneshot::channel();
        let job = Job::Update {
            list: list.clone(),
            change,
            reply,
        };
        self.jobs.send(job).await.map_err(|_| closed())?;
        answer.await.map_err(|_| closed())?
    }
}

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
