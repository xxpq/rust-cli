//! ← 打开任务列表的手势（docs/specs/rust-cli/tui-cockpit.md §3.5）。
//!
//! 修 bug（2026-11-12，用户报告“方向键左进入驾驶舱……生效的时机很奇怪，有时候有，有时候没有”）：以前草稿一空，← 就立刻
//! 打开或关上任务列表。按住 ← 时第一下打开，紧跟着的连发在任务列表里又是“回去”，开了又关；刚删空草稿时本想移动光标，整页
//! 却换了。这里只记时刻，判断都按按键处理时的时刻（`dispatch` 的 `now`），没有计时器；“再按一次”的时限是槽里那条提示自己的
//! `until`（`notice.rs` 的连按机制）。
use std::time::{Duration, Instant};

/// 两下 ← 不到这么久：键盘的连发，不算。
const REPEAT: Duration = Duration::from_millis(150);
/// 打开或回去之后这么久里的 ← 不算。
const SETTLE: Duration = Duration::from_millis(1_000);
/// 草稿在这么久里刚被删空：第一下只提示，再按一下才打开。
const EDITED: Duration = Duration::from_millis(2_000);
/// 提示“再按一次”留多久（3 s）。
pub const ARMED: Duration = Duration::from_millis(3_000);

/// 按键状态里的三个时刻：上一下 ←、上次打开或回去、草稿上次被删空。
#[derive(Debug, Default)]
pub struct CockpitGesture {
    last: Option<Instant>,
    fired: Option<Instant>,
    emptied: Option<Instant>,
}

fn within(at: Option<Instant>, now: Instant, span: Duration) -> bool {
    at.is_some_and(|at| now.saturating_duration_since(at) < span)
}

impl CockpitGesture {
    /// 一下会打开或关上任务列表的 ←（草稿空着）到了：连发或刚打开、回去之后的不算，返回 false。每一下都记下。
    pub fn admits(&mut self, now: Instant) -> bool {
        let last = self.last.replace(now);
        !(within(last, now, REPEAT) || within(self.fired, now, SETTLE))
    }

    /// 这一下打开或关上了任务列表。
    pub fn fired(&mut self, now: Instant) {
        self.fired = Some(now);
    }

    /// 别的键：之前的 ← 不再是按住不放的那一串，连发与刚打开之后的规则都不再算它（打开后按了 ⏎ 又马上按 ← 是新的一下）。
    pub fn other_key(&mut self) {
        self.last = None;
        self.fired = None;
    }

    /// 删除键把草稿删空了（退格、Delete、剪掉词或行；发送与 esc 清空不算）。
    pub fn emptied(&mut self, now: Instant) {
        self.emptied = Some(now);
    }

    /// 草稿刚被删空：这一下只提示“再按一次”。
    pub fn just_emptied(&self, now: Instant) -> bool {
        within(self.emptied, now, EDITED)
    }
}

#[cfg(test)]
#[path = "cockpit_gesture_tests.rs"]
mod tests;
