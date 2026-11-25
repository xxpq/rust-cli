//! 会话列表的 `live` 集合（04 §3、§4.1；protocol-v5.md §14.1、§20.2）：本进程里「正在跑」的会话——有回合在跑、有待回答的交互，
//! 或有后台工作在跑。
//! 事实全是这个会话的投影已有的（内核的回合、交互与后台工作的事件），这里只换成 `LiveRow` 的形状（tasks@1 的 `LiveRowSchema`）。
//! Host 的自动归档按它判「没跑完」；回合结束是行的移除，或会话仍 hot 时 `phase` 从 running 变成结局（04 §4.1）。

use serde_json::json;

use super::state::Outcome;
use crate::publisher::Fields;

/// 会话列表里的集合名（04 §3）。
pub(crate) const LIVE: &str = "live";

/// `LiveRow.phase`（`LIVE_PHASES`）。
const PHASE_IDLE: &str = "idle";
const PHASE_RUNNING: &str = "running";
const PHASE_COMPLETED: &str = "completed";
const PHASE_ERROR: &str = "error";
/// `LiveRow.runtime`（`LIVE_RUNTIMES`）。
const RUNTIME_READY: &str = "ready";
const RUNTIME_BUSY: &str = "busy";

/// 一个会话的活性：只有「正在跑」时有（在跑的回合、待回答的交互或在跑的后台工作）。
pub(crate) struct Hot<'a> {
    pub(crate) running: bool,
    /// 上一个回合的结局（回合不在跑、交互还在等时给 phase）。
    pub(crate) outcome: Option<Outcome>,
    /// 待回答的交互：内核的在前，前端持有的（请求安全校验，§35.2）在后。
    pub(crate) pending: Vec<Asked<'a>>,
    /// 有在跑的后台工作（`hasBackgroundWork`，protocol-v5.md §20.2）。
    pub(crate) background: bool,
}

/// 一条待回答的交互：实体 id、它在会话 topic 里的版本、种类与询问它的工具（前端持有的没有）。
pub(crate) struct Asked<'a> {
    pub(crate) id: &'a str,
    pub(crate) v: Option<u64>,
    pub(crate) kind: &'static str,
    pub(crate) tool: Option<&'a str>,
}

/// `LiveRow` 的字段（不含 id、v）。目标、工作流的活动内核还给不出（M9），按契约的缺省不写。
pub(crate) fn row(hot: &Hot<'_>) -> Fields {
    let phase = match (hot.running, hot.outcome) {
        (true, _) => PHASE_RUNNING,
        (false, Some(Outcome::Error)) => PHASE_ERROR,
        (false, Some(Outcome::Success | Outcome::Interrupted)) => PHASE_COMPLETED,
        (false, None) => PHASE_IDLE,
    };
    let mut fields = Fields::new();
    fields.insert("phase".to_owned(), json!(phase));
    fields.insert(
        "runtime".to_owned(),
        json!(if hot.running {
            RUNTIME_BUSY
        } else {
            RUNTIME_READY
        }),
    );
    fields.insert("hasBackgroundWork".to_owned(), json!(hot.background));
    fields.insert("pendingCount".to_owned(), json!(hot.pending.len()));
    if let Some(asked) = hot.pending.first() {
        let mut pending = json!({"id": asked.id, "kind": asked.kind});
        if let Some(tool) = asked.tool {
            pending["toolName"] = json!(tool);
        }
        if let Some(v) = asked.v {
            pending["v"] = json!(v);
        }
        fields.insert("pendingInteraction".to_owned(), pending);
    }
    fields
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
