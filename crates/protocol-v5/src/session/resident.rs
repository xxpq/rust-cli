//! 热会话按上限回收（protocol-v5.md §69；用户定 2026-11-11）：一条连接上同时开着运行时的会话最多 [`HOT_SESSIONS_MAX`] 个，超过时关最久没用、
//! 且空闲的。
//!
//! 热不热看投影（通道每次打开会话都交来活视图，§66.0）；「空闲」先由这里的预选挑候选（表项与投影上同步可读的事实），再由会话 actor 在它的一步里
//! 判定（`SessionHandle::close_idle`，判完即进入关闭，不留窗口）。不扫描、不加定时器：只在两处复查——通道交来活视图、会话从忙变闲
//! （actor 处理完一条通道的消息或一条运行时的读时比较前后）。

use serde_json::Value;

use super::Slot;
use super::lane::LaneRequest;
use crate::consts::HOT_SESSIONS_MAX;
use crate::errors::Fault;
use crate::server::Actor;

/// 一个会话在回收里的事实（会话表项上，actor 是 owner）。
#[derive(Debug, Default)]
pub(super) struct Residence {
    /// 最近一次用完（运行时从忙变闲）的序号：越小越久没用。
    used: u64,
    /// 已经交给通道关、还没有 `Closed`：不算热、不再是候选。
    closing: bool,
}

/// 处理一条消息之前看到的：会话运行时开着没有、预选说空闲没有。
#[derive(Debug, Clone, Copy)]
pub(super) struct Seen {
    runtime: bool,
    quiet: bool,
}

impl Seen {
    pub(super) fn of(slot: &Slot) -> Self {
        Self {
            runtime: runtime(slot),
            quiet: quiet(slot),
        }
    }
}

fn runtime(slot: &Slot) -> bool {
    slot.projection
        .as_ref()
        .is_some_and(super::projection::Projection::runtime)
}

/// actor 的预选（§69.0 第 3 条）：表项上没有在途的命令、在读的视图、在途的运行时的读，投影里看得到的工作都没有。
fn quiet(slot: &Slot) -> bool {
    slot.commands == 0
        && !slot.reading
        && slot.runtime_reads == 0
        && slot
            .projection
            .as_ref()
            .is_some_and(super::projection::Projection::quiet)
}

/// 算热的：运行时开着、不在关。
fn counted(slot: &Slot) -> bool {
    runtime(slot) && !slot.residence.closing
}

impl Actor {
    /// 处理完这个会话的一条消息：新激活（之前不热、现在热），或从忙变闲（「用」记成现在）时复查上限。
    pub(super) fn resident_after(&mut self, sid: &str, before: Seen) {
        let Some(slot) = self.sessions.slots.get_mut(sid) else {
            return;
        };
        let now = Seen::of(slot);
        let activated = !before.runtime && now.runtime;
        let idled = before.runtime && now.runtime && !before.quiet && now.quiet;
        if idled {
            self.sessions.uses += 1;
            slot.residence.used = self.sessions.uses;
        }
        if activated || idled {
            self.rebalance();
        }
    }

    /// 热的个数超过上限时关空闲的，关到不超过为止：先关没有订阅者的、再关有订阅者的（有人在看的排在后面），同一档里按「用」从早到晚
    /// （订阅不算「用」，§69.0 第 2 条）。空闲的不够时照常留着、记 warn（下一次复查再看）。
    fn rebalance(&mut self) {
        let hot = self
            .sessions
            .slots
            .values()
            .filter(|slot| counted(slot))
            .count();
        if hot <= HOT_SESSIONS_MAX {
            return;
        }
        let excess = hot - HOT_SESSIONS_MAX;
        let mut idle: Vec<(bool, u64, String)> = self
            .sessions
            .slots
            .iter()
            .filter(|(_, slot)| counted(slot) && quiet(slot))
            .map(|(sid, slot)| {
                let watched = self.outbox.watched(&slot.topic);
                (watched, slot.residence.used, sid.clone())
            })
            .collect();
        idle.sort();
        if idle.len() < excess {
            tracing::warn!(
                event = "protocol.session.hot_over_cap",
                hot,
                max = HOT_SESSIONS_MAX,
                idle = idle.len(),
                "more sessions keep their runtime than the cap and not enough of them are idle"
            );
        }
        for (_, _, sid) in idle.into_iter().take(excess) {
            let Some(slot) = self.sessions.slots.get_mut(&sid) else {
                continue;
            };
            // 热的会话一定有通道（通道交来的活视图建的投影）；发不出去只在进程关闭时
            if let Some((requests, _)) = &slot.lane
                && requests.send(LaneRequest::Close).is_ok()
            {
                slot.residence.closing = true;
            }
        }
    }

    /// 通道答了 `Close`（§69.0 第 6、7 条）：关了，投影变冷，再照 §66 复查释放；会话 actor 判出不空闲，会话留热、「用」记成现在（排到最后）。
    pub(super) fn closed(&mut self, sid: &str, cold: bool) {
        let Some(slot) = self.sessions.slots.get_mut(sid) else {
            return;
        };
        slot.residence.closing = false;
        if !cold {
            self.sessions.uses += 1;
            slot.residence.used = self.sessions.uses;
            return;
        }
        if let (Some(projection), Some(publisher)) =
            (slot.projection.as_mut(), self.topics.get_mut(&slot.topic))
            && publisher.publish(projection.went_cold())
        {
            self.outbox.mark_topic(&slot.topic);
        }
        self.release_unheld(sid);
    }

    /// 通道里的要运行时的读回来了（§43.2）：表项上的在途数减一（在途的读挡着回收，§69.3），再照常回复。
    pub(crate) async fn runtime_read(
        &mut self,
        sid: &str,
        reply: Value,
        result: Result<Value, Fault>,
    ) {
        if let Some(slot) = self.sessions.slots.get_mut(sid) {
            let before = Seen::of(slot);
            slot.runtime_reads -= 1;
            self.resident_after(sid, before);
        }
        self.session_read(reply, result).await;
    }
}

#[cfg(test)]
#[path = "resident_tests.rs"]
mod tests;
