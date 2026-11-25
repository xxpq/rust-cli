//! 会话里在跑的后台工作（K-W1，docs/specs/rust-cli/session-lifecycle.md §10.28）：会话 actor 的后台工作表。
//!
//! 开始来自主线程的工具结束（结果的 `display` 里的 [`BackgroundChange`]）与 `arm_monitor`；结束来自终态通知、monitor 的收尾
//! 与停止。表只在内存，是 actor 发出的 `BackgroundStarted` / `BackgroundEnded` 的唯一来源，视图的 `works` 取它。结束了的项记下终态
//! （§10.44），后台命令的输出详情在结束之后从这里取状态。

use std::collections::{BTreeSet, VecDeque};

use rust_cli_kernel_api::background::{BackgroundChange, BackgroundStart};
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::manage::BackgroundKind;
use rust_cli_kernel_api::session_api::KernelEvent;

use crate::actor::Actor;

/// 记下终态的结束了的项数（大小预算，不是计时器：同一个会话里同时打开着输出详情的后台工作不会多到这么多）。
pub(crate) const FINISHED_RETAIN: usize = 64;

/// 一项结束了的工作：种类、终态与说明（说明给它的通知开的回合用，§10.50：子代理的通知只带 ID）。
#[derive(Debug)]
struct Finished {
    id: String,
    kind: BackgroundKind,
    status: BackgroundStatus,
    description: String,
    /// 起它的工具调用：改写历史截掉这次调用时，它还没送达的终态通知不进新分支（§10.55）。
    tool_use: Option<ToolUseId>,
}

#[derive(Debug, Default)]
pub(crate) struct Works {
    /// 在跑的，按开始的先后。
    running: Vec<BackgroundStart>,
    /// 这一轮里终态先于开始到的 id（修 bug：开始在工具结束时才报，而后台命令或子代理可能在那之前就结束了，两路输入进
    /// actor 的先后不定；不记下它，迟到的开始会把一项早已结束的工作留在表里）。开始只来自本轮的工具结束，回合结束时清空。
    ended_early: BTreeSet<String>,
    /// 最近结束的，最新的在后（最多 [`FINISHED_RETAIN`] 项）。执行环境的账本在终态通知 ack 之后就不认识一条命令、停掉的命令
    /// 没有通知，结束之后的状态只有这里记着（修 bug：输出详情在命令结束之后读到「不认识」，看不到终态）。
    finished: VecDeque<Finished>,
    /// 改写历史截掉了起它们的工具调用的工作（§10.55，`ReplaceInput{stop_cut_works}`）：它们的终态通知 ack 了丢掉，不进新分支。只在内存；
    /// 子代理的通知到了就去掉，停掉的命令没有通知、留着（每次编辑几项）。
    fenced: BTreeSet<String>,
}

impl Works {
    /// 一项工作开始了：返回要发的事件；它在这一轮里已经结束（终态先到）时为 None。已在表里时也是 None：SendMessage 续跑一个
    /// 停放的子代理，它的通知还没送过，是同一项工作。
    pub(crate) fn started(&mut self, start: BackgroundStart) -> Option<KernelEvent> {
        if self.ended_early.remove(&start.id) || self.running.iter().any(|w| w.id == start.id) {
            return None;
        }
        self.running.push(start.clone());
        Some(KernelEvent::BackgroundStarted { work: start })
    }

    /// 一项工作结束了，终态是 `status`：返回要发的事件；表里没有它时为 None。`in_turn`：回合在跑，开始可能还在路上（记下它）。
    pub(crate) fn ended(
        &mut self,
        id: &str,
        status: BackgroundStatus,
        in_turn: bool,
    ) -> Option<KernelEvent> {
        match self.running.iter().position(|work| work.id == id) {
            Some(at) => {
                let work = self.running.remove(at);
                if self.finished.len() == FINISHED_RETAIN {
                    self.finished.pop_front();
                }
                self.finished.push_back(Finished {
                    id: work.id,
                    kind: work.kind,
                    status,
                    description: work.description,
                    tool_use: work.tool_use,
                });
                Some(KernelEvent::BackgroundEnded {
                    work: id.to_owned(),
                })
            }
            None => {
                if in_turn {
                    self.ended_early.insert(id.to_owned());
                }
                None
            }
        }
    }

    pub(crate) fn kind_of(&self, id: &str) -> Option<BackgroundKind> {
        self.running
            .iter()
            .find(|work| work.id == id)
            .map(|work| work.kind)
    }

    /// 一项工作现在的种类与状态：在跑的是 `Running`，结束了的是记下的终态；都没有（不认识、早已丢掉）时为 None。
    pub(crate) fn state(&self, id: &str) -> Option<(BackgroundKind, BackgroundStatus)> {
        if let Some(kind) = self.kind_of(id) {
            return Some((kind, BackgroundStatus::Running));
        }
        self.finished
            .iter()
            .rev()
            .find(|work| work.id == id)
            .map(|work| (work.kind, work.status))
    }

    /// 一项工作的说明：在跑的与记着的结束了的项都找；都没有时为 None。
    pub(crate) fn description(&self, id: &str) -> Option<&str> {
        self.running
            .iter()
            .find(|work| work.id == id)
            .map(|work| work.description.as_str())
            .or_else(|| {
                self.finished
                    .iter()
                    .rev()
                    .find(|work| work.id == id)
                    .map(|work| work.description.as_str())
            })
    }

    /// 回合结束：之后不会再有本轮工具结束带来的开始。
    pub(crate) fn turn_ended(&mut self) {
        self.ended_early.clear();
    }

    pub(crate) fn running(&self) -> Vec<BackgroundStart> {
        self.running.clone()
    }

    /// 没有在跑的后台工作（会话空闲的判据之一，session-lifecycle.md §3.3.2）。
    pub(crate) fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    /// 改写历史截掉了 `cut` 里的工具调用（§10.55）：它们起的工作（在跑的与结束了、通知可能还没到的）都记进围栏；返回在跑的，由调用方停。
    pub(crate) fn fence(&mut self, cut: &BTreeSet<ToolUseId>) -> Vec<BackgroundStart> {
        let launched_in_cut =
            |tool_use: &Option<ToolUseId>| tool_use.as_ref().is_some_and(|t| cut.contains(t));
        let running: Vec<BackgroundStart> = self
            .running
            .iter()
            .filter(|work| launched_in_cut(&work.tool_use))
            .cloned()
            .collect();
        let finished = self
            .finished
            .iter()
            .filter(|work| launched_in_cut(&work.tool_use))
            .map(|work| work.id.clone());
        let ids: Vec<String> = running
            .iter()
            .map(|work| work.id.clone())
            .chain(finished)
            .collect();
        self.fenced.extend(ids);
        running
    }

    pub(crate) fn fenced(&self, id: &str) -> bool {
        self.fenced.contains(id)
    }

    /// 围栏里的子代理的终态通知到了：之后不会再有它的通知。
    pub(crate) fn unfence(&mut self, id: &str) {
        self.fenced.remove(id);
    }
}

impl Actor {
    /// 主线程的工具结束：结果里说明了后台工作的变化（[`BackgroundChange::of`]），记进表并发事件（在这次的 `ToolDone` 之后）。
    pub(crate) fn tool_changed_work(&mut self, tool_use: &ToolUseId, change: BackgroundChange) {
        match change {
            BackgroundChange::Launched {
                id,
                kind,
                description,
                child,
            } => {
                let accepted = self.work_started(BackgroundStart {
                    id,
                    kind,
                    description,
                    tool_use: Some(tool_use.clone()),
                    child: child.clone(),
                    started_at: self.deps.clock.now_ms(),
                });
                // 子代理表跟着后台工作表（§10.56）：收下的开始让停放的子代理转后台、续跑的再跑；这一轮里它已经先结束（没收下）时不动
                if accepted && let Some(child) = child {
                    self.subagent_launched(&child);
                }
            }
            BackgroundChange::Stopped { id } => self.command_stopped(&id),
        }
    }

    /// 返回后台工作表收下了没有（这一轮里它已经结束、或已在表里时没有）。
    pub(crate) fn work_started(&mut self, start: BackgroundStart) -> bool {
        let event = self.works.started(start);
        let accepted = event.is_some();
        if let Some(event) = event {
            self.log.append(event);
        }
        accepted
    }

    pub(crate) fn work_ended(&mut self, id: &str, status: BackgroundStatus) {
        let in_turn = self.running.is_some();
        if let Some(event) = self.works.ended(id, status, in_turn) {
            self.log.append(event);
        }
    }

    /// 停了一条后台工作（用户的 StopTask、模型的 TaskStop）：只有后台命令停了就结束——停了的命令没有终态通知；子代理照常有
    /// `killed` 通知，monitor 的结束在它的事件流结束时（monitors.rs）。
    pub(crate) fn command_stopped(&mut self, id: &str) {
        if self.works.kind_of(id) == Some(BackgroundKind::Shell) {
            self.work_ended(id, BackgroundStatus::Killed);
        }
    }
}

#[cfg(test)]
#[path = "works_tests.rs"]
mod tests;
