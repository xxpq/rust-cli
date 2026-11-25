//! 冷会话的投影按最近使用保留（protocol-v5.md §70；用户定 2026-11-12）：最后一个持有者走了之后，由冷读建、之后没有命令经过通道的投影
//! 不释放，连同它的 Publisher 留着，最多 [`RETAINED_SESSIONS_MAX`] 个，超过时释放最久没用的。重订时通道读一次库里的会话事实，与保留时的比：
//! 相同就登记回去（epoch 不变），不同就照首次物化重建（新 epoch）。保留的会话不算物化：查询走临时投影、命令照没有投影走。
//!
//! ```text
//! 最后一个持有者走 ─ 带事实？─ 是 ─► 注销 topic，(Publisher, 投影, 事实, 序号) 进保留 ─ 超过上限 ─► 释放序号最小的
//!                             └ 否 ─► 释放（§66）
//! 订阅 ─ 保留着 ─► hold（不回复），通道 Check(事实) ─► Checked{same} ─ 是 ─► 登记回去 → 帧标记 → subscribed
//!                                                                 └ 否 ─► 丢掉保留的 → pending → 读视图（新 epoch）
//! ```
//!
//! 判据、进保留、挤出、复核的结果都在 actor 的一步里（同步片）；读事实在通道里。不定时刷新、不轮询。

use rust_cli_kernel_api::agents::StoredSubagent;
use rust_cli_kernel_api::session_facts::SessionFacts;
use rust_cli_kernel_api::store::StoreError;
use serde_json::Value;

use super::lane::LaneRequest;
use super::projection::Projection;
use super::{Sessions, Slot};
use crate::consts::RETAINED_SESSIONS_MAX;
use crate::generated::core_v1::{Cursor, Subscribe};
use crate::publisher::Publisher;
use crate::server::Actor;

/// 库里的戳（§70.0 第 2 条）：会话事实与分支切点（会话行的回退游标原文）。冷读与复核都在读快照之前读它；两份相同就是库里的这个会话没变：
/// 写消息、part、配置会推进事实里的活动时间或改字段，只回退的写入两边都不推进活动时间，所以另比分支切点。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stamp {
    facts: SessionFacts,
    cut: Option<String>,
}

impl Stamp {
    /// 读分支切点，与已经读到的会话事实合成戳。
    pub(super) async fn read(
        queries: &dyn rust_cli_kernel_api::store::StoreQueries,
        facts: SessionFacts,
    ) -> Result<Self, StoreError> {
        let cut = queries.branch_cut(&facts.id).await?;
        Ok(Self { facts, cut })
    }
}

/// 保留着的一份投影：topic 已注销，内容与它的 Publisher（epoch、seq、日志、骨架）都还在。
struct Kept {
    publisher: Publisher,
    projection: Projection,
    /// 建它的冷读读到的库里的戳：重订时与库里现在的比。
    stamp: Stamp,
    /// 进保留的序号：越小越久没用。
    order: u64,
}

/// 等复核的一个订阅请求（hold 着，不回复）。
struct Waiter {
    id: Value,
    sub: String,
    cursor: Option<Cursor>,
}

/// 一个会话在保留里的事实（会话表项上，actor 是 owner）。
#[derive(Default)]
pub(super) struct Retention {
    /// 当前投影（topic 登记着）与库里这一份戳相同：由冷读建、之后没有命令经过通道。None 是不知道（活视图建的、命令改过的、删除的）。
    stamp: Option<Stamp>,
    kept: Option<Kept>,
    waiters: Vec<Waiter>,
    /// 通道在读事实（复核在途）：挡着释放（同读视图，§66.2），`shutdown` 等它（在途的读）。
    checking: bool,
}

impl Retention {
    pub(super) fn checking(&self) -> bool {
        self.checking
    }

    /// 投影由视图建好了：冷读带来的戳记下，活视图没有。
    pub(super) fn built(&mut self, stamp: Option<Stamp>) {
        self.stamp = stamp;
    }

    /// 命令经过通道、或会话被删：当前投影不再是库里那一份的样子（不进保留，`session.subagents` 照旧经通道，§70.0 第 1、2 条）。
    pub(super) fn touched(&mut self) {
        self.stamp = None;
    }
}

impl Slot {
    /// 丢掉保留的（过时、挤出、删除、有了新的视图）：释放次数 + 1，之后建的投影换 epoch（§66.0 第 2 条）。
    pub(super) fn discard_kept(&mut self) {
        if self.retention.kept.take().is_some() {
            self.releases += 1;
        }
    }
}

impl Sessions {
    /// 当前投影由冷读建、没被命令改过时，它物化时读的库里的子代理（§70.0 第 1 条）：要会话运行时的读按冷的答，不经通道、不读库。
    pub(crate) fn cold_subagents(&self, sid: &str) -> Option<Vec<StoredSubagent>> {
        let slot = self.slots.get(sid)?;
        slot.retention.stamp.as_ref()?;
        slot.projection.as_ref().map(Projection::stored_subagents)
    }
}

impl Actor {
    /// §66 的判据说没有持有者了、topic 已注销：带事实的投影连同 Publisher 进保留（返回 true），否则交给调用方释放。
    pub(super) fn retain(&mut self, sid: &str, publisher: Option<Publisher>) -> bool {
        let Sessions {
            slots, retained, ..
        } = &mut self.sessions;
        let Some(slot) = slots.get_mut(sid) else {
            return false;
        };
        let (Some(publisher), Some(stamp)) = (publisher, slot.retention.stamp.take()) else {
            return false;
        };
        let Some(projection) = slot.projection.take() else {
            return false;
        };
        *retained += 1;
        let order = *retained;
        // 通道与调试记录照释放丢掉（投影已经拿走，释放次数不变：保留的 epoch 还会再用）
        slot.release();
        slot.retention.kept = Some(Kept {
            publisher,
            projection,
            stamp,
            order,
        });
        self.evict();
        true
    }

    /// 保留的超过上限：释放进保留最早的那个（§70.0 第 2 条）。每次进保留之后查一次，所以至多多出一个。
    fn evict(&mut self) {
        let kept: Vec<(u64, &String)> = self
            .sessions
            .slots
            .iter()
            .filter_map(|(sid, slot)| slot.retention.kept.as_ref().map(|kept| (kept.order, sid)))
            .collect();
        if kept.len() <= RETAINED_SESSIONS_MAX {
            return;
        }
        let Some(oldest) = kept.into_iter().min().map(|(_, sid)| sid.clone()) else {
            return;
        };
        if let Some(slot) = self.sessions.slots.get_mut(&oldest) {
            slot.discard_kept();
        }
    }

    /// 订阅一个保留着的会话（§70.0 第 2 条）：订阅 hold 着、记下请求，通道复核（同一时刻只复核一次）。不是保留着的返回 false，照首次物化走。
    pub(super) fn subscribe_kept(
        &mut self,
        id: &Value,
        subscribe: &Subscribe,
        key: &str,
        sid: &str,
    ) -> bool {
        let kept = self
            .sessions
            .slots
            .get(sid)
            .and_then(|slot| slot.retention.kept.as_ref())
            .map(|kept| kept.stamp.clone());
        let Some(known) = kept else {
            return false;
        };
        // 进保留时丢了通道：这里起一条（进程级部件一定建好了，保留的投影就是它们建的）
        let Some(requests) = self.lane(key, sid) else {
            return false;
        };
        let Some(slot) = self.sessions.slots.get_mut(sid) else {
            return false;
        };
        self.outbox
            .hold(&subscribe.sub, &slot.topic, subscribe.cursor.clone());
        slot.retention.waiters.push(Waiter {
            id: id.clone(),
            sub: subscribe.sub.clone(),
            cursor: subscribe.cursor.clone(),
        });
        if !slot.retention.checking && requests.send(LaneRequest::Check(Box::new(known))).is_ok() {
            slot.retention.checking = true;
            self.sessions.reads += 1;
        }
        true
    }

    /// 通道复核完了（§70.3）：相同且还保留着就登记回去；topic 在复核期间已由新的视图登记（命令打开了会话）就只回复；其余照首次物化走。
    pub(super) fn checked(&mut self, sid: &str, same: bool) {
        let Some(slot) = self.sessions.slots.get_mut(sid) else {
            return;
        };
        slot.retention.checking = false;
        self.sessions.reads -= 1;
        if !self.topics.contains_key(&slot.topic) {
            let reused = if same {
                slot.retention.kept.take()
            } else {
                None
            };
            match reused {
                Some(kept) => {
                    slot.projection = Some(kept.projection);
                    slot.retention.stamp = Some(kept.stamp);
                    self.topics.insert(slot.topic.clone(), kept.publisher);
                    // 登记即通知（01 §5.2）：带同一 epoch cursor 的订阅续传，别的拿快照
                    self.outbox.mark_topic(&slot.topic);
                }
                None => {
                    // 库里的会话变了（或保留的已被挤出）：丢掉保留的，照首次物化重建
                    slot.discard_kept();
                    let key = slot.key.clone();
                    self.waiters_pending(sid);
                    self.read(&key, sid);
                }
            }
        }
        self.answer_waiters(sid);
        // 复核期间订阅都走了：照 §66 复查（登记回去的马上又进保留）
        self.release_unheld(sid);
        self.finish_shutdown();
    }

    /// 复核读出拒绝（删除了、别的工作区、库不可用）：丢掉保留的，等着的订阅回 pending，之后与读视图失败一样收 `subscription.failed`
    /// （调用方接着排；`shutdown` 的回复排在它们之后）。
    pub(super) fn check_refused(&mut self, sid: &str) {
        let Some(slot) = self.sessions.slots.get_mut(sid) else {
            return;
        };
        if std::mem::take(&mut slot.retention.checking) {
            self.sessions.reads -= 1;
        }
        slot.discard_kept();
        self.waiters_pending(sid);
    }

    /// topic 登记了（复用保留的、或新的视图）：等复核的订阅回 `subscribed`，它们的帧标记已经排在前面。
    pub(super) fn answer_waiters(&mut self, sid: &str) {
        let Some(slot) = self.sessions.slots.get_mut(sid) else {
            return;
        };
        for Waiter { id, sub, cursor } in std::mem::take(&mut slot.retention.waiters) {
            self.outbox.subscribed(id, &sub, cursor);
        }
    }

    /// 保留的用不上了：等复核的订阅回 `pending`，之后照首次物化走（登记时拿快照，读不出时收 `subscription.failed`）。
    fn waiters_pending(&mut self, sid: &str) {
        let Some(slot) = self.sessions.slots.get_mut(sid) else {
            return;
        };
        for Waiter { id, sub, .. } in std::mem::take(&mut slot.retention.waiters) {
            self.outbox.pending(id, &sub);
        }
    }
}

#[cfg(test)]
#[path = "retain_tests.rs"]
mod tests;
