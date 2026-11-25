//! 压缩标记的一生（protocol-v5.md §17.3、§23.1、§47.1、§50；从 projection.rs 与 projection_lists.rs 分出，800 行上限）：手动压缩的回合开始时一行
//! `running`，边界落库时换成成功的标记（边界消息的 id），失败时同一行改成 `failed`，停止时改成 `cancelled`。活会话与由视图建走同一组函数。

use rust_cli_kernel_api::compaction::{LoopCompaction, LoopCompactionEnd};
use rust_cli_kernel_api::store::TurnRecord;

use super::*;
use crate::generated::session_v1::{CompactMarkerDetailOrigin, CompactMarkerDetailStatus};

/// 由视图建时按 `after` 放的主循环压缩（§50.2）：消息 → 排在它之后的（回合、位置、那一项）。
pub(super) type LoopMarkers<'a> =
    BTreeMap<&'a MessageId, Vec<(&'a TurnId, usize, &'a LoopCompaction)>>;

/// 由视图建投影时走消息的先后：存储的先后（protocol-v5.md §62.1）。冷读的历史是活动形状，压缩点的保留段（边界的 `preserved`，
/// `head..=tail`）接在摘要之后（kernel-api `history::active`）；回合记录只锚每个回合的第一条消息、后面的按走的先后归回合，
/// 所以先把保留段挪回边界之前。活会话写的压缩，保留段本来就在边界之前，在边界之后找不到首尾，不动。
///
/// 修 bug（R-COMPACT-TAIL-COLD）：以前照视图的先后走，冷读里手动压缩保留的尾段归到压缩的回合、排在压缩标记之后，与活会话的行不同。
/// 从最后一个边界往前还原：后一次压缩再保留上一次的保留段时，冷读里它只在最后一个摘要之后，先还原后一个，前一个才在自己的边界之后找得到它。
/// 只往前挪，较早的边界的下标不变。
pub(super) fn stored_order(shown: &[Message]) -> Vec<&Message> {
    let mut order: Vec<&Message> = shown.iter().collect();
    let boundaries: Vec<usize> = (0..order.len())
        .filter(|&at| preserved(order[at]).is_some())
        .collect();
    for boundary in boundaries.into_iter().rev() {
        let Some(segment) = preserved(order[boundary]) else {
            continue;
        };
        let after = |id: &MessageId| {
            order[boundary + 1..]
                .iter()
                .position(|message| message.id() == id)
                .map(|at| boundary + 1 + at)
        };
        // 首尾都在边界之后才是冷读的形状；`tail` 在 `head` 之前时不是一段（同 `history::active`）
        let (Some(head), Some(tail)) = (after(&segment.head), after(&segment.tail)) else {
            continue;
        };
        if tail < head {
            continue;
        }
        let moved: Vec<&Message> = order.drain(head..=tail).collect();
        order.splice(boundary..boundary, moved);
    }
    order
}

/// 压缩边界的保留段（没有保留段、不是边界时为 None）。
fn preserved(message: &Message) -> Option<&rust_cli_kernel_api::message::PreservedSegment> {
    match message {
        Message::System(rust_cli_kernel_api::message::SystemMessage::CompactBoundary(boundary)) => {
            boundary.preserved.as_ref()
        }
        _ => None,
    }
}

/// 回合记录里主循环的压缩按 `after` 分好（同一条消息之后的几项按开始的先后）。
pub(super) fn loop_markers(turns: &[TurnRecord]) -> LoopMarkers<'_> {
    let mut markers = LoopMarkers::new();
    for record in turns {
        for (index, compaction) in record.compactions.iter().enumerate() {
            markers
                .entry(&compaction.after)
                .or_default()
                .push((&record.turn, index, compaction));
        }
    }
    markers
}

impl Projection {
    /// 回合头之后：在跑的回合是压缩时出开始的标记（`status: running`，§47.1）。活会话的 `TurnStarted` 与由视图建的在跑回合都经回合头的同一个
    /// 函数到这里，所以两边是同一行。别的回合、已结束的回合不出。
    ///
    /// 修 bug（19 §47 R-COMPACT-STARTED）：以前压缩的回合在跑时没有压缩标记（只在结束时有），界面没有「压缩中」。
    pub(super) fn compaction_started(
        &mut self,
        edit: &mut Edit<'_>,
        start: &TurnStart,
        state: &str,
    ) {
        if start.origin == TurnOrigin::Compaction && state == turn_state::RUNNING {
            let turn = (
                start.turn.as_str(),
                start.started_at,
                start.command.as_ref(),
            );
            self.pending_marker(edit, turn, CompactMarkerDetailStatus::Running);
        }
    }

    /// 成功的标记（边界消息的 id，§15.3.3）的行号。这次压缩的开始的标记在时，它让位：先去掉，成功的标记接着用它的行号——同一个位置、
    /// 同一批（§47.1，同 v4 一次压缩占同一行）。冷读里没有开始的标记，按显示顺序编出的也是这个号，所以活会话与冷读同值。开始的标记是
    /// 主循环里在跑的那一项（§50.2）的，或者压缩的回合的。
    pub(super) fn boundary_seq(&mut self, edit: &mut Edit<'_>, turn: &str, id: &str) -> u64 {
        let pending = self
            .running
            .as_ref()
            .and_then(|running| running.loop_compaction)
            .map_or_else(
                || rows::turn_compaction_id(turn),
                |index| rows::loop_compaction_id(turn, index),
            );
        let taken = edit
            .get(ROWS, &pending)
            .and_then(|fields| fields.get(CREATED_AT_SEQ).and_then(Value::as_u64))
            .filter(|_| edit.get(ROWS, id).is_none());
        match taken {
            Some(seq) => {
                self.remove_row(edit, &pending);
                self.order.insert((seq, id.to_owned()));
                seq
            }
            None => self.seq_of(edit, id),
        }
    }

    /// 在跑的压缩回合结束（活会话的 `TurnFinished`）：失败 → 同一行改成 `failed`（§23.1）；中断 → 同一行改成 `cancelled`（§50.1）；成功时边界
    /// 早已换掉它。
    ///
    /// 修 bug（protocol-v5.md §50.1）：以前中断时去掉这一行，因为契约里没有 `cancelled`；v4 显示「已取消」，主会话审定照 v4，契约已加这个值。
    pub(super) fn compaction_ended(
        &mut self,
        edit: &mut Edit<'_>,
        running: &Running,
        outcome: Outcome,
    ) {
        if !running.compact {
            return;
        }
        let turn = (
            running.turn.as_str(),
            running.started_at,
            running.command.as_ref(),
        );
        match outcome {
            Outcome::Error => self.pending_marker(edit, turn, CompactMarkerDetailStatus::Failed),
            Outcome::Interrupted => {
                self.pending_marker(edit, turn, CompactMarkerDetailStatus::Cancelled);
            }
            Outcome::Success => {}
        }
    }

    /// 冷读里一个已结束的回合：来源是压缩、结局是失败或被中断时出失败或取消的压缩标记（§23.1、§50.1）。回合记录的来源、开始、命令与结局就是
    /// 活会话里那几样事实，所以冷读与活会话出同一行。
    pub(super) fn recorded_compaction(&mut self, edit: &mut Edit<'_>, record: &TurnRecord) {
        let status = match record.ended.as_ref().map(|ended| ended.outcome) {
            Some(InputEnd::Failed) => CompactMarkerDetailStatus::Failed,
            Some(InputEnd::Cancelled) => CompactMarkerDetailStatus::Cancelled,
            Some(InputEnd::Completed) | None => return,
        };
        if record.origin == TurnOrigin::Compaction {
            let turn = (
                record.turn.as_str(),
                record.started_at,
                record.command.as_ref(),
            );
            self.pending_marker(edit, turn, status);
        }
    }

    /// 开始、失败与取消的标记（同一行 `<回合 id>:compact`，§47.1、§50.1）：已有时沿用它的行号，原地改；带回合的命令（同成功的标记，§24.1）。`turn` 是
    /// （回合 id、开始的时刻、开它的命令）：取自回合的开始、在跑的回合或回合记录，三处同值。
    fn pending_marker(
        &mut self,
        edit: &mut Edit<'_>,
        (turn, started_at, command): (&str, u64, Option<&CommandId>),
        status: CompactMarkerDetailStatus,
    ) {
        let id = rows::turn_compaction_id(turn);
        let seq = self.seq_of(edit, &id);
        let mut fields = rows::pending_compaction(
            turn,
            seq,
            Some(started_at),
            CompactMarkerDetailOrigin::Manual,
            status,
        );
        rows::set_command(&mut fields, command);
        edit.put(ROWS, &id, fields);
    }

    /// 主循环里的一次压缩的标记（§50.2）：活会话的 `CompactionRecorded` 与由视图建都经这里，所以两边是同一行。在跑 → `running`，失败、
    /// 停止 → 同一行原地改；成功的不出行（边界行已接过它的行号）；没有结局、回合也不在跑的（进程在中途退出）不出。没有命令（v4 的自动压缩也没有）。
    ///
    /// 修 bug（protocol-v5.md §50.2）：以前主循环里的压缩在跑、失败与停止时都没有标记（内核没有这个事实），只在成功时有边界行；v4 都有。
    pub(super) fn loop_compaction(
        &mut self,
        edit: &mut Edit<'_>,
        (turn, index): (&str, usize),
        compaction: &LoopCompaction,
        running: bool,
    ) {
        let status = match compaction.ended {
            None if running => CompactMarkerDetailStatus::Running,
            Some(LoopCompactionEnd::Failed) => CompactMarkerDetailStatus::Failed,
            Some(LoopCompactionEnd::Cancelled) => CompactMarkerDetailStatus::Cancelled,
            None | Some(LoopCompactionEnd::Succeeded) => return,
        };
        let id = rows::loop_compaction_id(turn, index);
        let seq = self.seq_of(edit, &id);
        let fields = rows::pending_compaction(
            turn,
            seq,
            Some(compaction.started_at),
            rows::marker_origin(compaction.trigger),
            status,
        );
        edit.put(ROWS, &id, fields);
    }

    /// 活会话的 `CompactionRecorded`（§50.2）：在跑的回合记下在跑的那一项（它的边界提交时让位），再出它的行。
    pub(super) fn loop_recorded(
        &mut self,
        edit: &mut Edit<'_>,
        turn: &TurnId,
        index: usize,
        compaction: &LoopCompaction,
    ) {
        let running = match self.running.as_mut() {
            Some(running) if running.turn == *turn => {
                running.loop_compaction = compaction.ended.is_none().then_some(index);
                true
            }
            _ => false,
        };
        self.loop_compaction(edit, (turn.as_str(), index), compaction, running);
    }
}

#[cfg(test)]
#[path = "projection_compact_tests.rs"]
mod tests;
