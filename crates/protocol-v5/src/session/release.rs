//! 会话投影的释放与重建（protocol-v5.md §66；02 §8.2）：冷投影在没有持有者时释放，再需要时照首次物化的路从存储重建、换 epoch。
//!
//! 释放是 actor 一步里的一个同步片：判据读的都是 actor 里同步可读的当前事实（订阅归 Outbox，运行时开没开经 `Viewed` 交给投影，
//! 在途的命令与读视图记在会话表项上），判据、注销 topic、改表项之间没有 `.await`。不扫描、不加定时器、不设宽限期：每个持有者结束时复查一次。

use super::projection::Projection;
use super::{Slot, debug::DebugLog};
use crate::outbox::Outbox;
use crate::server::Actor;
use crate::topics::{self, Topic};

/// 释放过的会话重建时 epoch 里的一段（`<进程 epoch>:session:<释放次数>`，02 §8.2）：不可能等于进程 epoch 与临时投影的 epoch（`:read:`，§40.2）。
const REBUILT_EPOCH: &str = ":session:";

/// 这个会话 topic 的 epoch：第一次物化用进程 epoch（行为不变）；释放过的用新 epoch，带旧 cursor 的订阅与查询一定拿快照或 stale。
pub(super) fn topic_epoch(process: &str, releases: u64) -> String {
    if releases == 0 {
        process.to_owned()
    } else {
        format!("{process}{REBUILT_EPOCH}{releases}")
    }
}

/// 还有持有者（§66.2）：订阅（含等物化的）、在途的命令、在读的视图或在复核的事实（§70）、开着的会话运行时（热会话按上限回收，§69）。
pub(super) fn held(slot: &Slot, outbox: &Outbox) -> bool {
    outbox.watched(&slot.topic)
        || slot.commands > 0
        || slot.reading
        || slot.retention.checking()
        || slot.projection.as_ref().is_some_and(Projection::runtime)
}

impl Slot {
    /// 丢投影、通道与调试记录；工作区（一个会话只属于一个工作区，§15.1）与释放次数留着。丢了投影才算释放过一次：没有登记过 topic 的
    /// 表项（只跑过命令）没有 client 拿着它的 cursor，不用换 epoch。
    pub(super) fn release(&mut self) {
        if self.projection.take().is_some() {
            self.releases += 1;
        }
        self.retention.touched();
        // 通道里没有在途的请求（判据），发送端丢掉之后它的任务读到结束就退出
        self.lane = None;
        self.debug = DebugLog::default();
    }
}

impl Actor {
    /// 一个订阅离开了 `topic`（退订，或同一个 sub 改订别处，§66.0 第 4 条 ①）：是会话 topic 就复查。
    pub(crate) fn left_topic(&mut self, topic: &str) {
        if let Ok(Topic::WorkspaceSession { sid, .. }) = topics::parse(topic) {
            self.release_unheld(&sid);
        }
    }

    /// 没有持有者就释放：注销 topic（此刻没有订阅，不发帧）、丢投影与通道。冷投影没有 `live` 行（§14.1）与前端持有的待校验（§35），
    /// 没有要撤的（§66.2）。由冷读建、没被命令改过的投影先进保留（§70），不释放。
    pub(super) fn release_unheld(&mut self, sid: &str) {
        let Some(slot) = self.sessions.slots.get(sid) else {
            return;
        };
        if held(slot, &self.outbox) {
            return;
        }
        let publisher = self.topics.remove(&slot.topic);
        if self.retain(sid, publisher) {
            return;
        }
        if let Some(slot) = self.sessions.slots.get_mut(sid) {
            slot.release();
        }
    }
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod tests;
