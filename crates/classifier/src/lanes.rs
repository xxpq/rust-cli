//! 按（会话，代理）排队（docs/specs/rust-cli/auto-classifier.md §15.8）：同一个代理的分类一个一个来，按到达先后。
//!
//! 每个键一个只有一个许可的信号量（tokio 的信号量按到达先后给许可）。表在锁里只插入与删除，拿到许可之后才 await 请求；
//! 最后一个使用者离开时删掉表项。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// 一条队列：会话与代理（主线程是 None）。
pub(crate) type LaneKey = (SessionId, Option<AgentId>);

#[derive(Default)]
pub(crate) struct Lanes(Mutex<HashMap<LaneKey, Arc<Semaphore>>>);

/// 排到了：拿着许可直到放掉（作用域结束）。
pub(crate) struct Turn<'a> {
    lanes: &'a Lanes,
    key: LaneKey,
    semaphore: Arc<Semaphore>,
    permit: Option<OwnedSemaphorePermit>,
}

impl Lanes {
    /// 排队；等的时候取消了就是 None（不发请求）。
    pub(crate) async fn enter(&self, key: LaneKey, cancel: &CancellationToken) -> Option<Turn<'_>> {
        let semaphore = self
            .0
            .lock_unpoisoned()
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone();
        // 先建好守卫：等的时候 future 被丢掉（或取消）也会经它清掉表项
        let mut turn = Turn {
            lanes: self,
            key,
            semaphore,
            permit: None,
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            permit = turn.semaphore.clone().acquire_owned() => {
                // 信号量不会关闭：表项只在没有使用者时删掉
                turn.permit = permit.ok();
                Some(turn)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.lock_unpoisoned().len()
    }
}

/// 表里一份、这个守卫一份：没有别的使用者（等着的、拿着许可的）。
const ONLY_THE_TABLE_AND_US: usize = 2;

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.permit.take();
        let mut lanes = self.lanes.0.lock_unpoisoned();
        if Arc::strong_count(&self.semaphore) == ONLY_THE_TABLE_AND_US {
            lanes.remove(&self.key);
        }
    }
}

#[cfg(test)]
#[path = "lanes_tests.rs"]
mod tests;
