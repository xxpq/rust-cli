//! 投影里的工具行（从 projection.rs 分出，800 行上限）：开始、运行中的输出预览（protocol-v5.md §19.4）、结局、开工与结束的时刻（§31）。
//!
//! 修 bug（TOOL-TIME，§31）：以前工具行不写 `startedAt` / `endedAt`：内核的事件不带时刻，冷读也不交 tool part 的 `state.time`。现在开工的时刻来自
//! `ToolStarted`，起止随结果的提交（`ToolTimeRecorded`，由视图建时是视图的 `tool_times`）同一个函数写；回合结束时还没有结局的行按回合的结束收口。

use rust_cli_kernel_api::metering::ToolTime;
use rust_cli_kernel_api::store::TurnRecord;

use super::*;

impl Projection {
    pub(super) fn tool_started(
        &mut self,
        edit: &mut Edit<'_>,
        tool_use: &ToolUseId,
        (name, input): (&str, &Value),
        started_at: u64,
    ) {
        let id = tool_use.as_str();
        let turn = self
            .running
            .as_ref()
            .map(|r| r.turn.as_str().to_owned())
            .unwrap_or_default();
        let seq = self.seq_of(edit, id);
        let mut fields = edit
            .get(ROWS, id)
            .cloned()
            .unwrap_or_else(|| rows::tool_call(tool_status::RUNNING, "", (&turn, seq, None)));
        if tool_status::ended(rows::text(&fields, keys::STATUS)) {
            return;
        }
        if rows::text(&fields, keys::STATUS) != tool_status::PENDING_APPROVAL {
            rows::set_status(&mut fields, tool_status::RUNNING);
        }
        fields.insert(keys::TOOL_NAME.to_owned(), json!(name));
        fields.insert(keys::INPUT.to_owned(), input.clone());
        // 在跑的行先有开工的时刻（界面算已运行的时间）；与随结果落库的 `started_at` 是执行器同一次读时钟
        fields.insert(keys::STARTED_AT.to_owned(), rows::millis(started_at));
        self.open_tools.insert(id.to_owned(), turn);
        edit.put(ROWS, id, fields);
    }

    /// 调用的开工与结束（§31.1）：活会话的 `ToolTimeRecorded` 与由视图建时视图的 `tool_times` 同一个函数。行不在（回退掉的分支）时不动。
    pub(super) fn timed(&self, edit: &mut Edit<'_>, time: &ToolTime) {
        edit.update(ROWS, time.tool_use.as_str(), |fields| {
            fields.insert(keys::STARTED_AT.to_owned(), rows::millis(time.started_at));
            fields.insert(keys::ENDED_AT.to_owned(), rows::millis(time.ended_at));
        });
    }

    /// 回合结束时还没有结局的工具行（02 §11 C7）：已取消，结束的时刻是回合的结束（§31.1，照 v4 的 `closeOpenToolRows`）。活会话的
    /// `TurnFinished` 与由视图建时已结束的回合同一个函数；没有结束的时刻（旧的回合记录）不写。
    pub(super) fn close_unsettled(&mut self, edit: &mut Edit<'_>, id: &str, ended_at: Option<u64>) {
        self.open_tools.remove(id);
        self.previews.remove(id);
        edit.update(ROWS, id, |fields| {
            rows::set_status(fields, tool_status::CANCELLED);
            fields.remove(keys::APPROVAL);
            // 同 `result`：有了结局就去掉运行中的预览（§19.4）
            fields.remove(keys::OUTPUT_PREVIEW);
            if let Some(at) = ended_at {
                fields.insert(keys::ENDED_AT.to_owned(), rows::millis(at));
            }
        });
    }

    /// 由视图建时，冷读里没有结果的工具：会话不在跑这个回合时它不会再有结果（续写时内核给未结调用补中断结果），按它所在回合的回合记录收口。
    pub(super) fn close_recorded_unsettled(&mut self, edit: &mut Edit<'_>, turns: &[TurnRecord]) {
        let running = self.running.as_ref().map(|r| r.turn.as_str().to_owned());
        for (id, turn) in std::mem::take(&mut self.open_tools) {
            if Some(&turn) == running.as_ref() {
                self.open_tools.insert(id, turn);
                continue;
            }
            let ended_at = turns
                .iter()
                .find(|record| record.turn.as_str() == turn)
                .and_then(|record| record.ended.as_ref()?.time)
                .map(|time| time.ended_at);
            self.close_unsettled(edit, &id, ended_at);
        }
    }

    /// 运行中的工具的一段输出（§19.4）：接进这个工具的尾部缓冲，写成工具行的 `outputPreview`。行不在、或已有结局时不动
    /// （结局之后日志里不会再有它的进度）。
    pub(super) fn progressed(&mut self, edit: &mut Edit<'_>, id: &str, output: &str, total: u64) {
        let Some(mut fields) = edit.get(ROWS, id).cloned() else {
            return;
        };
        if tool_status::ended(rows::text(&fields, keys::STATUS)) {
            return;
        }
        let preview = self
            .previews
            .entry(id.to_owned())
            .or_default()
            .push(output, total);
        fields.insert(keys::OUTPUT_PREVIEW.to_owned(), preview);
        edit.put(ROWS, id, fields);
    }

    /// 工具的结局。`display` 是只给界面的数据（活会话的 `ToolDone` 带，§21）；结果消息（冷读、或排在 `ToolDone` 之后）没有它。
    pub(super) fn result(
        &mut self,
        edit: &mut Edit<'_>,
        id: &str,
        text: &str,
        is_error: bool,
        display: Option<Value>,
    ) {
        if edit.get(ROWS, id).is_none() {
            return;
        }
        self.open_tools.remove(id);
        // 有了结局就去掉运行中的预览：它只在运行中显示，冷读没有它，留着活会话与冷读的行就不同（§15.3.3、§19.4）
        self.previews.remove(id);
        edit.update(ROWS, id, |fields| {
            fields.remove(keys::OUTPUT_PREVIEW);
            // 已读成「已取消」的（停止时 `ToolDone{cancelled}` 先到，或由视图建时视图说它取消了）：结果消息只补输出，状态不退回「出错」
            let cancelled = rows::text(fields, keys::STATUS) == tool_status::CANCELLED
                || self.cancelled.contains(id);
            rows::set_result(fields, text, is_error, display);
            self.with_plan(id, fields);
            if cancelled {
                rows::set_status(fields, tool_status::CANCELLED);
            }
            fields.remove(keys::APPROVAL);
        });
    }
}

#[cfg(test)]
#[path = "projection_tools_tests.rs"]
mod tests;
