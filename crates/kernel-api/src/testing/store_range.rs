//! [`MemoryStore`] 的范围读（[`crate::store::StoreQueries::range`]，docs/specs/rust-cli/node-store.md §5.6）：整段读出之后按显示顺序选段、只留这一段的
//! 消息与事实。显示顺序 = `earlier ++ messages` 里每条内核消息所在的存储消息（[`Message::record`]），相邻同一条只算一次；选段的规则是
//! [`HistorySpan::ranges`]（与 node-store 同一个，没有记录的历史同样按开回合的输入补段首）。

use std::collections::HashSet;

use crate::agents::StoredSubagent;
use crate::ids::{MessageId, ToolUseId};
use crate::message::Message;
use crate::store::{HistoryRange, HistorySpan, LoadedSession, StoreError};

pub(super) fn range(
    loaded: LoadedSession,
    subagents: Vec<StoredSubagent>,
    span: &HistorySpan,
) -> Result<HistoryRange, StoreError> {
    let mut order: Vec<MessageId> = Vec::new();
    for message in loaded.earlier.iter().chain(&loaded.messages) {
        if order.last() != Some(message.record()) {
            order.push(message.record().clone());
        }
    }
    // 回合记录的 `first` 是内核消息的 id（压缩回合从边界开头，边界的存储消息是它的摘要）：换成它所在的存储消息
    let record_of = |first: &MessageId| -> MessageId {
        loaded
            .earlier
            .iter()
            .chain(&loaded.messages)
            .find(|message| message.id() == first)
            .map_or_else(|| first.clone(), |message| message.record().clone())
    };
    let opens: HashSet<MessageId> = loaded
        .turns
        .iter()
        .map(|turn| record_of(&turn.first))
        .collect();
    // 调用过这个工具的存储消息（`Calling`）
    let calling: HashSet<&MessageId> = match span {
        HistorySpan::Calling { tool, .. } => loaded
            .earlier
            .iter()
            .chain(&loaded.messages)
            .filter(|message| match message {
                Message::Assistant(assistant) => assistant.content.iter().any(|block| {
                    matches!(block, crate::message::ContentBlock::ToolUse { name, .. } if name == tool)
                }),
                _ => false,
            })
            .map(Message::record)
            .collect(),
        _ => HashSet::new(),
    };
    // 没有记录的历史按开回合的输入切（node-store.md §5.6）：同 node-store，判这条存储消息换出的内核消息
    let unrecorded = |at: usize| -> Result<bool, std::convert::Infallible> {
        Ok(loaded
            .earlier
            .iter()
            .chain(&loaded.messages)
            .any(|message| message.record() == &order[at] && crate::turn_walk::opens_turn(message)))
    };
    let Ok(ranges) = span.ranges(
        &order,
        |id| opens.contains(id),
        |id| calling.contains(id),
        unrecorded,
    );
    let ranges = ranges.ok_or_else(|| StoreError::NotFound {
        session: loaded.meta.id.clone(),
    })?;
    let earlier = matches!(span, HistorySpan::Before { .. })
        && ranges.first().is_some_and(|range| range.start > 0);
    let picked: HashSet<&MessageId> = ranges
        .iter()
        .flat_map(|range| &order[range.clone()])
        .collect();
    let keep = |messages: &[Message]| -> Vec<Message> {
        messages
            .iter()
            .filter(|message| picked.contains(message.record()))
            .cloned()
            .collect()
    };
    let (messages, shown) = (keep(&loaded.messages), keep(&loaded.earlier));
    let calls: HashSet<ToolUseId> = shown
        .iter()
        .chain(&messages)
        .flat_map(Message::tool_uses)
        .cloned()
        .collect();
    let picked_ids: HashSet<MessageId> = picked.into_iter().cloned().collect();
    let floor = ranges
        .first()
        .filter(|range| !range.is_empty())
        .map(|range| order[range.start].clone());
    Ok(HistoryRange {
        floor,
        subagents: subagents
            .into_iter()
            .filter(|stored| calls.contains(&stored.tool_use))
            .collect(),
        earlier,
        loaded: LoadedSession {
            turns: loaded
                .turns
                .into_iter()
                .filter(|turn| picked_ids.contains(&record_of(&turn.first)))
                .collect(),
            inputs: loaded
                .inputs
                .into_iter()
                .filter(|input| picked_ids.contains(&input.message))
                .collect(),
            sources: loaded
                .sources
                .into_iter()
                .filter(|source| picked_ids.contains(&source.message))
                .collect(),
            output_heads: loaded
                .output_heads
                .into_iter()
                .filter(|head| calls.contains(&head.tool_use))
                .collect(),
            tool_times: loaded
                .tool_times
                .into_iter()
                .filter(|time| calls.contains(&time.tool_use))
                .collect(),
            messages,
            earlier: shown,
            ..loaded
        },
    })
}
