//! 投影里前端持有的交互（protocol-v5.md §35.2；从 projection.rs 分出，800 行上限）：待回答的 `requestVerification`。内核不认识它，
//! 它不在内核的视图里，所以投影重建（激活、改写历史）时从旧投影带过来、并进新内容。删除会话时它与内核的交互一起清掉（`deleted_now`）。

use std::collections::BTreeMap;

use super::*;
use crate::session::verify::Pending;

/// 前端持有的待回答的校验：实体 id → 字段与应答口。
pub(crate) type Verifications = BTreeMap<String, Pending>;

impl Projection {
    /// 删除之后（`session.delete` 的回执）：`phase: deleted`，待回答的交互清空，准入拒绝一切（02 §4.2）。
    pub(crate) fn deleted_now(&mut self, publisher: &Publisher) -> Batch {
        self.deleted = true;
        self.running = None;
        let mut edit = Edit::new(Some(publisher));
        // 前端持有的校验一并清掉：应答口随之丢掉，取凭据的任务以「没被应答就结束了」失败（§35.2）
        let front = std::mem::take(&mut self.verifications).into_keys();
        for id in std::mem::take(&mut self.interactions)
            .into_keys()
            .chain(front)
        {
            edit.remove(INTERACTIONS, &id);
        }
        self.control(&mut edit);
        edit.state(state::keys::ADMISSION, self.admission());
        edit.into_batch()
    }

    /// 这个实体是前端持有的校验。
    pub(crate) fn verifying(&self, id: &str) -> bool {
        self.verifications.contains_key(id)
    }

    /// 记下一个待回答的校验（实体由 actor 写进 topic）。
    pub(crate) fn hold(&mut self, id: &str, pending: Pending) {
        self.verifications.insert(id.to_owned(), pending);
    }

    /// 拿走一个待回答的校验（应答或撤回）；不在时为 None。
    pub(crate) fn release(&mut self, id: &str) -> Option<Pending> {
        self.verifications.remove(id)
    }

    /// 待回答的校验的 id（会话删除时索引一并移除）。
    pub(crate) fn verification_ids(&self) -> Vec<String> {
        self.verifications.keys().cloned().collect()
    }

    /// 这个会话在待校验索引里的行。
    pub(crate) fn verification_rows(&self, sid: &str) -> Vec<(String, Fields)> {
        self.verifications
            .iter()
            .map(|(id, pending)| (id.clone(), pending.row(sid)))
            .collect()
    }

    /// 投影重建之前取出前端交互（新投影由内核的视图建，视图里没有它们）。
    pub(crate) fn take_verifications(&mut self) -> Verifications {
        std::mem::take(&mut self.verifications)
    }

    /// 投影重建之后放回前端交互，实体并进新内容（21 §13.4 第 14 行）：快照重建换了它们的 `v`，裁决按 id，不受影响。
    pub(crate) fn keep_verifications(&mut self, kept: Verifications, content: &mut Content) {
        if kept.is_empty() {
            return;
        }
        let interactions = content
            .collections
            .entry(INTERACTIONS.to_owned())
            .or_default();
        for (id, pending) in &kept {
            interactions.insert(id.clone(), pending.entity());
        }
        self.verifications = kept;
    }
}
