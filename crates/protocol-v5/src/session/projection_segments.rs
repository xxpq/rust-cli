//! 回合头的工作段与回合的结束（protocol-v5.md §26.2、§29.1，照 v4 的 product projection）：插话行开一段，同一次取出（上一条插话之后还没有
//! assistant 消息）的后续插话并入第一条开的那一段；回合结束时回合头写结束的时刻与耗时，最后一段收口。活会话与冷读都经这里，时刻取自回合头的
//! `startedAt`、插话消息的创建时刻与内核的 `TurnTime`，两边同值。
//!
//! 修 bug（M06f）：以前回合头不写 `workSegments`，v5 的 `RowSchema` 读成 `[]`，界面按 v4 的规则只在数组里的段首切、空数组就不切，
//! 插话之后的工作没有自己的「已工作」。修 bug（TURN-TIME）：以前回合头不写结束时刻与耗时（`durations` 是缺省值 0，界面显示「1 秒」），
//! 最后一段也没有结束，界面显示不带时长的「已处理」；内核的回合结束没有时刻，冷读给不出同一个值。

use rust_cli_kernel_api::session_api::TurnTime;
use rust_cli_kernel_api::store::TurnRecord;
use serde::Serialize;

use super::*;

/// 回合头里的字段（`turnHeader.workSegments`，v4 `turnWorkSegmentSchema` 的形状）。
const WORK_SEGMENTS: &str = "workSegments";
/// 回合开头那一段的 id：`<回合 id>` 加这个后缀（同 v4）。
const INITIAL_SEGMENT_SUFFIX: &str = ":initial";

/// 一段工作（v4 `TurnWorkSegment`）。还开着的段没有结束：下一段开始或回合结束时收口。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Segment {
    segment_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger_entity_id: Option<String>,
    started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    ended_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_ms: Option<u64>,
}

impl Segment {
    fn opened(segment_id: String, trigger_entity_id: Option<String>, started_at: u64) -> Self {
        Self {
            segment_id,
            trigger_entity_id,
            started_at,
            ended_at: None,
            active_ms: None,
        }
    }

    /// 在 `at` 结束（v4 `completeWorkSegments`：`activeMs = endedAt − startedAt`，墙上时间）。
    fn close(&mut self, at: u64) {
        self.ended_at = Some(at);
        self.active_ms = Some(at.saturating_sub(self.started_at));
    }
}

/// 当前回合的开始、结束与工作段（回合头建好时换一份；行的内容在 Publisher 里，这里只记推导下一批要的）。
#[derive(Debug, Default)]
pub(super) struct Segments {
    /// 回合开始的时刻（回合头的 `startedAt`）。回合中途建的投影没有，这时不开段。
    started_at: Option<u64>,
    /// 回合结束的时刻（§29.1）：活会话在 `TurnFinished` 时、由视图建时在有结局的回合记录处记下。最后一段还开着就按它收口。
    ended_at: Option<u64>,
    segments: Vec<Segment>,
    /// 上一条插话之后还没有 assistant 消息：后续的插话是同一次取出，并入同一段。
    draining: bool,
}

impl Segments {
    pub(super) fn new(started_at: Option<u64>) -> Self {
        Self {
            started_at,
            ..Self::default()
        }
    }

    /// 写进回合头的段：回合已结束时最后一段按结束的时刻收口。开段与回合结束都经这里，所以活会话与冷读写出同一组段。
    fn value(&self) -> Value {
        let mut segments = self.segments.clone();
        if let (Some(ended), Some(last)) = (self.ended_at, segments.last_mut())
            && last.ended_at.is_none()
        {
            last.close(ended);
        }
        json!(segments)
    }
}

impl Projection {
    /// 一条消息对工作段的影响（`message()` 在建行之前调）：assistant 关上「这一次取出」。
    pub(super) fn segment_message(&mut self, message: &Message) {
        if matches!(message, Message::Assistant(_)) {
            self.segments.draining = false;
        }
    }

    /// 一行用户输入（`message()` 建行时调）：插话、且不在同一次取出里时，回合头开一段；上一段在这一刻结束。
    pub(super) fn segment_input(
        &mut self,
        edit: &mut Edit<'_>,
        turn: &str,
        (input, guided, at): (&str, bool, Option<u64>),
    ) {
        let opens = guided && !self.segments.draining;
        self.segments.draining = guided;
        let (true, Some(started), Some(at)) = (opens, self.segments.started_at, at) else {
            return;
        };
        let Some(mut header) = edit.get(ROWS, turn).cloned() else {
            return;
        };
        let segments = &mut self.segments.segments;
        if segments.is_empty() {
            segments.push(Segment::opened(
                format!("{turn}{INITIAL_SEGMENT_SUFFIX}"),
                None,
                started,
            ));
        }
        if let Some(last) = segments.last_mut() {
            last.close(at);
        }
        segments.push(Segment::opened(
            input.to_owned(),
            Some(input.to_owned()),
            at,
        ));
        header.insert(WORK_SEGMENTS.to_owned(), self.segments.value());
        edit.put(ROWS, turn, header);
    }

    /// 由视图建时，已结束的回合（§29.1、§42.1）：回合头带回合记录里结束的时刻与用时、这个回合的文件改动，之后这个回合开的段按它收口。较早写的记录没有，不写。
    pub(super) fn recorded_end(&mut self, edit: &mut Edit<'_>, record: &TurnRecord) {
        let turn = record.turn.as_str();
        let (Some(ended), Some(mut header)) =
            (record.ended.as_ref(), edit.get(ROWS, turn).cloned())
        else {
            return;
        };
        // 旧记录没有结束的时刻（§29.1）与文件改动（§42.1）：没有就不写
        if let Some(time) = ended.time {
            self.turn_ended(&mut header, time);
        }
        rows::set_files(&mut header, &ended.files);
        edit.put(ROWS, turn, header);
    }

    /// 回合结束（活会话的 `TurnFinished`、由视图建时回合记录里的结局，§29.1）：回合头写结束的时刻与耗时；有段时最后一段在同一个 put 里收口。
    pub(super) fn turn_ended(&mut self, header: &mut Fields, time: TurnTime) {
        self.segments.ended_at = Some(time.ended_at);
        // 回合头建好时记下了开始（`Segments::new`），所以有回合头就有它
        if let Some(started) = self.segments.started_at {
            rows::set_ended(header, started, time);
        }
        if !self.segments.segments.is_empty() {
            header.insert(WORK_SEGMENTS.to_owned(), self.segments.value());
        }
    }
}

#[cfg(test)]
#[path = "projection_segments_tests.rs"]
mod tests;
