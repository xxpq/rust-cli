//! `MemoryStore` 的分叉要带上的记录（从 store.rs 分出，800 行上限）：复制了的消息的回合记录与输入记录（session-lifecycle.md §10.18）与它们的
//! 调用的计划（§10.57）。内存里 ID 不换，原样带上（同 node-store 的 `fork_clone::copy_records`）。

use std::collections::HashSet;

use crate::ids::{MessageId, ToolUseId};
use crate::message::Message;
use crate::store::{Fact, TurnRecord, entry_kinds};

pub(super) fn copied_records(facts: &[Fact], copied: &[Message]) -> Vec<Fact> {
    let kept: HashSet<&MessageId> = copied.iter().map(Message::id).collect();
    let calls: HashSet<&str> = copied
        .iter()
        .flat_map(Message::tool_uses)
        .map(ToolUseId::as_str)
        .collect();
    facts
        .iter()
        .filter(|fact| match fact {
            Fact::Entry { kind, id, data } if kind == entry_kinds::TURN => {
                TurnRecord::from_entry(id, data).is_some_and(|record| kept.contains(&record.first))
            }
            Fact::Entry { kind, id, .. } if kind == entry_kinds::INPUT => {
                kept.contains(&MessageId::new(id.as_str()))
            }
            Fact::Entry { kind, id, .. } if kind == entry_kinds::PLAN => {
                calls.contains(id.as_str())
            }
            _ => false,
        })
        .cloned()
        .collect()
}
