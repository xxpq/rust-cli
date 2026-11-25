//! 投影里的后台工作（K-W1，protocol-v5.md §20.1；从 projection.rs 分出，800 行上限）：内核的后台工作表 → `works` 集合。
//! 开始 = `entity.put`，结束 = `entity.remove`；由视图建投影时取视图的 `works`（同一个函数写一项）。

use rust_cli_kernel_api::background::BackgroundStart;

use super::*;
use crate::generated::session_v1::WorkStatus;

impl Projection {
    /// 一项后台工作开始了：`seq` 是这一批的 seq（`startedAtSeq`）；由视图建时是 0（同 `control.activeWorks`，§15.3.1）。
    pub(super) fn work_started(&mut self, edit: &mut Edit<'_>, work: &BackgroundStart, seq: u64) {
        self.works.insert(work.id.clone());
        edit.put(WORKS, &work.id, fields(work, seq));
    }

    /// 一项后台工作结束了：投影里没有它（内核在它的开始之前就见到了结束，session-lifecycle.md §10.28）时不发。
    pub(super) fn work_ended(&mut self, edit: &mut Edit<'_>, id: &str) {
        if self.works.remove(id) {
            edit.remove(WORKS, id);
        }
    }

    /// 有在跑的后台工作（会话列表的 `live`，§20.2）。
    pub(super) fn has_works(&self) -> bool {
        !self.works.is_empty()
    }
}

/// `WorkSchema` 的字段（不含 id、v）。
fn fields(work: &BackgroundStart, seq: u64) -> Fields {
    let kind = rows::work_kind(work.kind);
    // 三种都能停（work.cancel → StopTask，§20.3）
    let mut detail = json!({ "cancellable": true });
    if let Some(child) = &work.child {
        detail["childSessionId"] = json!(child.as_str());
    }
    let mut fields: Fields = [
        ("kind", json!(kind.as_str())),
        ("status", json!(WorkStatus::Running.as_str())),
        ("title", json!(work.description)),
        ("startedAtSeq", json!(seq)),
        ("startedAt", json!(work.started_at)),
        ("detail", detail),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    // 工具行的 id 就是 tool_use（§15.3.3）
    if let Some(tool_use) = &work.tool_use {
        fields.insert("anchorRowId".to_owned(), json!(tool_use.as_str()));
    }
    fields
}

#[cfg(test)]
#[path = "projection_works_tests.rs"]
mod tests;
