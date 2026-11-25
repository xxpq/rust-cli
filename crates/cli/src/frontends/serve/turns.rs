//! serve 进程里正在跑、或跑完还没取走的轮次（docs/specs/rust-cli/multi-tenant.md §4.3）。
//!
//! 一轮的输出只归它的 [`TurnLog`]：headless 写出的每一行加上 seq 进缓冲，连接从缓冲里取，所以连接断开不影响这一轮。
//! 断开后宽限内可以续上（`session_attach`，从某个 seq 之后接着收）；宽限到了还没有连接时，没跑完就打断，跑完就丢掉。
//! 宽限是“没人要的一轮”占用资源的上限，不用来同步状态：是否过期只看这期间有没有连接续上（`generation`）。
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use rust_cli_base::sync::LockUnpoisoned;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// 一轮的输出缓冲最多这么多字节（同运行壳的轮次日志）：超出时丢最早的行，续上时要的行已经丢了就回 `events_truncated`。
pub(crate) const MAX_BUFFER_BYTES: usize = 32 * 1024 * 1024;

/// 一轮的键：同一个用户的 `turn_id`。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TurnKey {
    pub(crate) user: String,
    pub(crate) turn_id: String,
}

/// 进程里的轮次表。
#[derive(Default)]
pub(crate) struct Turns {
    open: Mutex<HashMap<TurnKey, Arc<TurnLog>>>,
}

impl Turns {
    /// 登记一轮；同一个键已经在时不登记（`turn_exists`）。
    pub(crate) fn begin(&self, key: TurnKey) -> Option<Arc<TurnLog>> {
        let mut open = self.open.lock_unpoisoned();
        if open.contains_key(&key) {
            return None;
        }
        let log = Arc::new(TurnLog::new(key.clone()));
        open.insert(key, log.clone());
        Some(log)
    }

    pub(crate) fn find(&self, key: &TurnKey) -> Option<Arc<TurnLog>> {
        self.open.lock_unpoisoned().get(key).cloned()
    }

    /// 这一轮的输出已经交完或没人要了：拿掉。
    pub(crate) fn end(&self, key: &TurnKey) {
        self.open.lock_unpoisoned().remove(key);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.open.lock_unpoisoned().len()
    }
}

/// 一轮的进度：最后一行的 seq，跑完了没有。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Progress {
    pub(crate) last: u64,
    pub(crate) finished: bool,
}

/// 从某个 seq 之后取行的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Since {
    Lines(Vec<(u64, String)>),
    /// 要的那些行有的已经从缓冲里丢了。
    Truncated,
}

/// 宽限到了时要做的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Expiry {
    /// 这期间有连接续上：什么都不做。
    Kept,
    /// 没跑完：打断，跑完后由这一轮自己拿掉。
    Interrupt,
    /// 已经跑完：拿掉。
    Drop,
}

/// 一轮的输出缓冲与它的连接计数。
pub(crate) struct TurnLog {
    key: TurnKey,
    state: Mutex<State>,
    progress: watch::Sender<Progress>,
    /// 用户中断（同 `-p` 收到 SIGINT）：先于 `stop` 取消，回合按中断结算。
    pub(crate) cancel: CancellationToken,
    pub(crate) stop: CancellationToken,
}

#[derive(Default)]
struct State {
    lines: VecDeque<(u64, String)>,
    bytes: usize,
    last: u64,
    finished: bool,
    /// 正连着的连接数。
    attached: usize,
    /// 每次有连接接上加一：宽限到时比对它，判断这期间有没有连接续上。
    generation: u64,
    /// 宽限到了而没人续上。
    expired: bool,
}

impl TurnLog {
    fn new(key: TurnKey) -> Self {
        Self {
            key,
            state: Mutex::new(State::default()),
            progress: watch::Sender::new(Progress::default()),
            cancel: CancellationToken::new(),
            stop: CancellationToken::new(),
        }
    }

    pub(crate) fn key(&self) -> &TurnKey {
        &self.key
    }

    /// 加一行；`line` 用分到的 seq 写出这一行（加上 `turn_id` 与 `seq`）。
    pub(crate) fn push(&self, line: impl FnOnce(u64) -> String) {
        let progress = {
            let mut state = self.state.lock_unpoisoned();
            state.last += 1;
            let seq = state.last;
            let text = line(seq);
            state.bytes += text.len();
            state.lines.push_back((seq, text));
            while state.bytes > MAX_BUFFER_BYTES && state.lines.len() > 1 {
                if let Some((_, dropped)) = state.lines.pop_front() {
                    state.bytes -= dropped.len();
                }
            }
            Progress {
                last: state.last,
                finished: state.finished,
            }
        };
        self.progress.send_replace(progress);
    }

    /// 这一轮跑完了。宽限已经到了（没人会来取）时返回 true，由调用方拿掉这一轮。
    pub(crate) fn finish(&self) -> bool {
        let (progress, expired) = {
            let mut state = self.state.lock_unpoisoned();
            state.finished = true;
            (
                Progress {
                    last: state.last,
                    finished: true,
                },
                state.expired,
            )
        };
        self.progress.send_replace(progress);
        expired
    }

    /// `after` 之后的行。
    pub(crate) fn since(&self, after: u64) -> Since {
        let state = self.state.lock_unpoisoned();
        let first = state.lines.front().map_or(state.last + 1, |(seq, _)| *seq);
        if after + 1 < first {
            return Since::Truncated;
        }
        Since::Lines(
            state
                .lines
                .iter()
                .filter(|(seq, _)| *seq > after)
                .cloned()
                .collect(),
        )
    }

    pub(crate) fn watch(&self) -> watch::Receiver<Progress> {
        self.progress.subscribe()
    }

    /// 一个连接接上。
    pub(crate) fn attach(&self) {
        let mut state = self.state.lock_unpoisoned();
        state.attached += 1;
        state.generation += 1;
    }

    /// 一个连接断开；断开后没有连接了时给出这时的代次，调用方据此在宽限到时判断。
    pub(crate) fn detach(&self) -> Option<u64> {
        let mut state = self.state.lock_unpoisoned();
        state.attached = state.attached.saturating_sub(1);
        (state.attached == 0).then_some(state.generation)
    }

    /// 宽限到了（断开时的代次是 `generation`）。
    pub(crate) fn expire(&self, generation: u64) -> Expiry {
        let mut state = self.state.lock_unpoisoned();
        if state.attached > 0 || state.generation != generation {
            return Expiry::Kept;
        }
        state.expired = true;
        if state.finished {
            Expiry::Drop
        } else {
            Expiry::Interrupt
        }
    }

    /// 打断这一轮（同 `-p` 收到 SIGINT：先中断、再停止）。
    pub(crate) fn interrupt(&self) {
        self.cancel.cancel();
        self.stop.cancel();
    }
}

#[cfg(test)]
#[path = "turns_tests.rs"]
mod tests;
