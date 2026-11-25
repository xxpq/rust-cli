//! 投影里哪些行要留整行（protocol-v5.md §67.0 第 2 条）：窗口之外，其余的行只留骨架（排序、种类与回合），内容要时从存储重建。
//! 规则只此一处；Publisher 只管换（`Publisher::thin`）。

use rust_cli_kernel_api::tool_names::{AGENT, EXIT_PLAN_MODE};

use super::*;
use crate::outbox::Outbox;

/// 骨架留的键：排序（`createdAtSeq`）、种类与回合（`turn.retry`、`turn.fork` 判目标行）。
pub(crate) const OUTLINE: &[&str] = &[CREATED_AT_SEQ, keys::KIND, keys::TURN_ID];

impl Projection {
    /// 这一行要留整行：还会变的（没有结局、在跑的回合里的），与命令、查询要整行的（回合头、用户输入、计划、子代理与派生它的 Agent 调用），
    /// 与库里没有、重建不出的（hook 行）。
    pub(crate) fn resident(&self, fields: &Fields) -> bool {
        let text = |key: &str| rows::text(fields, key);
        let stable = fields.get(keys::STABLE).and_then(Value::as_bool) == Some(true);
        let running = self
            .running
            .as_ref()
            .is_some_and(|running| running.turn.as_str() == text(keys::TURN_ID));
        let kind = text(keys::KIND);
        // hook 行（§71.0 第 4 条）：事实只在会话 actor 的内存里（视图的 `hooks`），库里没有，换成骨架之后无法从库重建
        let pinned = matches!(kind, kinds::TURN_HEADER | kinds::USER_INPUT)
            || kind == subagent_rows::KIND
            || kind == hook_rows::KIND
            || (kind == kinds::TOOL_CALL
                && matches!(text(keys::TOOL_NAME), EXIT_PLAN_MODE | AGENT));
        !stable || running || pinned
    }
}

/// 窗口外不要整行的行换成骨架（§67.0 第 3 条）：建投影之后与回合结束之后各扫一遍。只换每个在订订阅都已收到的行（§68.0 第 5 条）：
/// 落后的订阅续传时要从当前内容拼它们的 put。
pub(crate) fn thin(publisher: &mut Publisher, projection: &Projection, outbox: &Outbox) {
    let sent = outbox.sent_floor(publisher.topic(), &publisher.cursor());
    publisher.thin(ROWS, OUTLINE, sent, |fields| projection.resident(fields));
}

#[cfg(test)]
#[path = "projection_residence_tests.rs"]
mod tests;
