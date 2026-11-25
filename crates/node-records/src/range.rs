//! 历史的一段（docs/specs/rust-cli/node-store.md §5.6）：前端冷读只读最后一屏要的那段、往前翻页与问题导航的目录按段读。
//! 不读整段的 part：消息头只读 message 表的 id、role、summary，压缩摘要与它的保留段读 part 判分支，段里的消息才整份读。选出的消息与
//! [`crate::load::load`] 里同一段的那几条相同（同一组 `piece`、`assemble`、`invalidated`），段的划分在 [`HistorySpan::ranges`]（回合的范围同建投影时的归属）。
//! 与段无关的部分收成 [`HistoryIndex`]：调用方在库没变时把上一次的交回来（node-store.md §5.7），下一段就不再读消息表。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::{Message, SystemMessage};
use rust_cli_kernel_api::store::{HistoryRange, HistorySpan, StoreError};
use serde_json::{Value, json};

use crate::branch::{Branch, active_messages, earlier_messages, invalidated};
use crate::facts::live_session;
use crate::hydrate::{assemble, piece};
use crate::raw::{MessageHead, RawRecord};
use crate::row::WithParts;
use crate::rows::{Rows, SessionRow};

/// 显示顺序里的一项在哪一边：只供显示的 `earlier` 还是模型侧的 `active`，与它在那一边的位置（同一边按它排回那一边的顺序）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Side {
    Earlier(usize),
    Active(usize),
}

/// 显示顺序里的一项除 id 之外的事实。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shown {
    side: Side,
    /// 模型侧保留段的副本（分支里的 `Cow::Owned`）：读出之后按 `invalidated` 改过再换成内核消息。
    copy: bool,
    /// 角色是 user：只有它可能是开回合的输入（段首往前补到没有记录的回合的开头时只解码它们，node-store.md §5.6）。
    user: bool,
}

/// 历史索引（node-store.md §5.7）：范围读里与段无关的部分。只依赖库里的消息、摘要与保留段的 part、回退游标；库没变时可以拿去读下一段。
#[derive(Debug, PartialEq)]
pub struct HistoryIndex {
    /// 显示顺序里每一项的存储消息 id（[`HistorySpan::ranges`] 的 `order`；回退保留的同一条可能出现两次）。
    ids: Vec<MessageId>,
    /// 与 `ids` 一一对应。
    shown: Vec<Shown>,
    /// 压缩边界的内核 id → 换出它的摘要那条存储消息的 id（回合记录的 `first` 是内核消息的 id）。
    boundaries: HashMap<String, String>,
}

/// 一次范围读的结果：这一段、checkpoint（调用方按 [`crate::rewind::Active`] 补出，同 [`crate::load::load`]）与这次用的索引（调用方按库的版本记住）。
pub struct Read {
    pub range: HistoryRange,
    pub checkpoints: crate::rewind::Checkpoints,
    pub index: Arc<HistoryIndex>,
}

/// 范围读；`checkpoints` 与 `reverted` 留空，由调用方按 [`crate::rewind::Active`] 补出（同 [`crate::load::load`]）。`index` 是同一库版本里
/// 上一次读给出的索引（没有就建）：调用方保证库在两次读之间没有变（node-store.md §5.7）。
pub fn read(
    conn: &dyn Rows,
    session: &SessionId,
    span: &HistorySpan,
    index: Option<Arc<HistoryIndex>>,
) -> Result<Read, StoreError> {
    let row = live_session(conn, session)?;
    let index = match index {
        Some(index) => index,
        None => Arc::new(HistoryIndex::build(conn, session, &row)?),
    };
    let turns = crate::load::turn_records(conn, session)?;
    // 回合记录的 `first` 是内核消息的 id：压缩回合从压缩边界开头，边界由摘要那条存储消息换出（`hydrate_boundary`），按存储消息找要换回摘要的 id。
    // 修 bug（NAV-DIR 合入前的 session-compact-tail-cold）：以前直接拿 `first` 当存储消息找，压缩回合的开头与回合记录都对不上，冷读少一个回合头、
    // 压缩标记归到上一个回合
    let record_of = |first: &MessageId| -> MessageId {
        index
            .boundaries
            .get(first.as_str())
            .map_or_else(|| first.clone(), |summary| MessageId::new(summary.as_str()))
    };
    let opens: HashSet<MessageId> = turns.iter().map(|turn| record_of(&turn.first)).collect();
    // 调用过这个工具的存储消息（下沿之前的计划，§73.4）：库里按 JSON 成员筛 part，只在 `Calling` 时读
    let calling: HashSet<String> = match span {
        HistorySpan::Calling { tool, .. } => conn
            .tool_parts(session.as_str(), &[tool.as_str()])?
            .into_iter()
            .map(|(message, _)| message)
            .collect(),
        _ => HashSet::new(),
    };
    // 没有记录的历史按开回合的输入切（node-store.md §5.6）：段首往前找时，只解码 user 的存储消息、换成内核消息判
    let unrecorded = |at: usize| -> Result<bool, StoreError> {
        if !index.shown[at].user {
            return Ok(false);
        }
        let raw = conn.messages_raw_of(session.as_str(), &[index.ids[at].as_str()])?;
        raw.first().map_or(Ok(false), |raw| {
            let record = raw.decode().map_err(crate::storage)?;
            Ok(piece(&record).opens_turn())
        })
    };
    let ranges = span
        .ranges(
            &index.ids,
            |id| opens.contains(id),
            |id| calling.contains(id.as_str()),
            unrecorded,
        )?
        .ok_or_else(|| StoreError::NotFound {
            session: session.clone(),
        })?;
    let picked: Vec<usize> = ranges.iter().flat_map(Clone::clone).collect();
    let picked_ids: HashSet<&str> = picked.iter().map(|&at| index.ids[at].as_str()).collect();
    // 段里的消息整份读一次（同一条在分支里出现两次时也只读一次），按存储的行序
    let wanted: Vec<&str> = picked_ids.iter().copied().collect();
    let mut stored: Vec<WithParts> = Vec::new();
    let (mut output_heads, mut tool_times, mut calls) = (Vec::new(), Vec::new(), Vec::new());
    for raw in conn.messages_raw_of(session.as_str(), &wanted)? {
        let decoded = raw.decode().map_err(crate::storage)?;
        for part in decoded.parts.iter().filter(|part| part["type"] == "tool") {
            output_heads.extend(crate::tool_parts::head_of(part));
            tool_times.extend(crate::tool_parts::time_of(part));
        }
        stored.push(decoded);
    }
    let full: HashMap<&str, &WithParts> =
        stored.iter().map(|record| (record.id(), record)).collect();
    // 两边各按自己的顺序换成内核消息（同 load 的 stream）：模型侧的保留段副本照样 invalidated，上下文用量按它算
    let kernel = |active: bool| -> Vec<Message> {
        let mut side: Vec<(usize, usize)> = picked
            .iter()
            .filter_map(|&at| match index.shown[at].side {
                Side::Active(rank) if active => Some((rank, at)),
                Side::Earlier(rank) if !active => Some((rank, at)),
                _ => None,
            })
            .collect();
        side.sort_unstable();
        let pieces = side
            .into_iter()
            .filter_map(|(_, at)| {
                let id = index.ids[at].as_str();
                let record = full.get(id)?;
                let piece = if index.shown[at].copy {
                    piece(&invalidated(record))
                } else {
                    piece(record)
                };
                Some((id, piece))
            })
            .collect();
        assemble(pieces)
    };
    let messages = kernel(true);
    let shown = kernel(false);
    // 段里主线程的 Agent 调用（同 subagents::stored 的取法：按活动分支的先后、同一子会话后写的覆盖）
    for &at in &picked {
        if let Some(decoded) = full.get(index.ids[at].as_str()) {
            crate::subagents::add_calls(&mut calls, decoded);
        }
    }
    let subagents = crate::subagents::resolve(conn, calls)?;
    let earlier_left = matches!(span, HistorySpan::Before { .. })
        && ranges.first().is_some_and(|range| range.start > 0);
    let in_range = |id: &MessageId| picked_ids.contains(id.as_str());
    let loaded = rust_cli_kernel_api::store::LoadedSession {
        turns: turns
            .into_iter()
            .filter(|turn| in_range(&record_of(&turn.first)))
            .collect(),
        inputs: crate::load::input_records(conn, session)?
            .into_iter()
            .filter(|input| in_range(&input.message))
            .collect(),
        sources: crate::load::sources(&stored),
        output_heads,
        tool_times,
        messages,
        earlier: shown,
        ..crate::load::session_level(conn, session, &row)?
    };
    let floor = ranges
        .first()
        .filter(|range| !range.is_empty())
        .map(|range| index.ids[range.start].clone());
    let range = HistoryRange {
        loaded,
        subagents,
        earlier: earlier_left,
        floor,
    };
    Ok(Read {
        range,
        checkpoints: crate::rewind::checkpoints(conn, session)?,
        index,
    })
}

impl HistoryIndex {
    /// 建索引：消息头（不建整份 info）、摘要与保留段的骨架、回退游标 → 分支（同 load）→ 显示顺序。
    fn build(conn: &dyn Rows, session: &SessionId, row: &SessionRow) -> Result<Self, StoreError> {
        let records = skeleton(conn, session)?;
        let branch = Branch::from_revert(row.revert.as_ref());
        let active = active_messages(&records, &branch, true);
        let earlier = earlier_messages(&records, &branch, &active);
        let order = display(&earlier, &active);
        let entry = |side: Side| match side {
            Side::Earlier(at) => &earlier[at],
            Side::Active(at) => &active[at],
        };
        Ok(Self {
            ids: order
                .iter()
                .map(|side| MessageId::new(entry(*side).id()))
                .collect(),
            shown: order
                .iter()
                .map(|&side| Shown {
                    side,
                    copy: matches!(entry(side), Cow::Owned(_)),
                    user: entry(side).info["role"] == "user",
                })
                .collect(),
            boundaries: boundary_records(&records),
        })
    }
}

/// 分支要的骨架：每条消息只有消息头（id 与 role，摘要另带 `summary`），不读 part；压缩摘要（`summary` 为真）与它保留段里的消息另读 part、
/// 只留 compaction 的（[`RawRecord::skeleton`]），用整份 info（保留段要它判能不能保留）。压缩边界只写在摘要上（Node 与本工程的写法，node-store.md §4），
/// 时间线 assistant 的 compaction part 带 `timelineStatus`、不是边界，所以分支与读全部 part 的骨架上选出的相同；保留段里的消息要看有没有 compaction
/// part（`preservable`）。
fn skeleton(conn: &dyn Rows, session: &SessionId) -> Result<Vec<WithParts>, StoreError> {
    let heads = conn.message_heads(session.as_str())?;
    let summaries: Vec<&str> = heads
        .iter()
        .filter(|head| head.summary)
        .map(|head| head.id.as_str())
        .collect();
    let mut index: Vec<WithParts> = heads.iter().map(head_record).collect();
    let mut read = skeleton_records(conn, session, &summaries)?;
    let position = |id: &str| heads.iter().position(|head| head.id == id);
    let mut members: Vec<&str> = Vec::new();
    for summary in &summaries {
        let segment = read
            .get(*summary)
            .and_then(|record| {
                record.parts.iter().find(|part| {
                    part["type"] == "compaction" && part["compactBoundary"].is_object()
                })
            })
            .map(|part| &part["compactBoundary"]["preservedSegment"]);
        let bound = |key: &str| {
            segment
                .and_then(|segment| segment[key].as_str())
                .and_then(position)
        };
        if let (Some(head), Some(tail)) = (bound("headMessageId"), bound("tailMessageId"))
            && head <= tail
        {
            members.extend(heads[head..=tail].iter().map(|head| head.id.as_str()));
        }
    }
    members.retain(|id| !read.contains_key(*id));
    let more = skeleton_records(conn, session, &members)?;
    read.extend(more);
    for record in &mut index {
        if let Some(found) = read.remove(record.id()) {
            *record = found;
        }
    }
    Ok(index)
}

/// 只有消息头的记录：分支读 id 与 role，摘要读 `summary`（显示顺序判压缩边界，[`segment`]）。
fn head_record(head: &MessageHead) -> WithParts {
    let mut info = json!({ "id": head.id });
    if let Some(role) = &head.role {
        info["role"] = Value::from(role.as_str());
    }
    if head.summary {
        info["summary"] = Value::Bool(true);
    }
    WithParts {
        info,
        parts: Vec::new(),
    }
}

/// 压缩边界的内核 id → 换出它的摘要那条存储消息的 id（骨架里摘要的 compaction part 的 `compactBoundary.boundaryId`）。
fn boundary_records(index: &[WithParts]) -> HashMap<String, String> {
    index
        .iter()
        .filter_map(|record| {
            let boundary = record
                .parts
                .iter()
                .find(|part| part["type"] == "compaction" && part["compactBoundary"].is_object())?;
            let id = boundary["compactBoundary"]["boundaryId"].as_str()?;
            Some((id.to_owned(), record.id().to_owned()))
        })
        .collect()
}

/// 这些消息的骨架（整份 info，part 只留 compaction 的）。
fn skeleton_records(
    conn: &dyn Rows,
    session: &SessionId,
    ids: &[&str],
) -> Result<HashMap<String, WithParts>, StoreError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    conn.messages_raw_of(session.as_str(), ids)?
        .iter()
        .map(|raw| {
            let skeleton = RawRecord::skeleton(raw).map_err(crate::storage)?;
            Ok((raw.id().to_owned(), skeleton))
        })
        .collect()
}

/// 显示顺序（前端建投影时的 `stored_order` 在消息这一层的同一规则）：只供显示的在前、模型侧的在后，再把每个压缩边界之后找得到的保留段（首到尾）
/// 挪回那个摘要之前；从最后一个边界往前还原，只往前挪，较早的边界的下标不变。边界 = 摘要上换得出带保留段的压缩边界（同冷读的 `hydrate_boundary`）。
fn display(earlier: &[Cow<'_, WithParts>], active: &[Cow<'_, WithParts>]) -> Vec<Side> {
    let mut order: Vec<Side> = (0..earlier.len())
        .map(Side::Earlier)
        .chain((0..active.len()).map(Side::Active))
        .collect();
    let record = |side: Side| match side {
        Side::Earlier(at) => &*earlier[at],
        Side::Active(at) => &*active[at],
    };
    let boundaries: Vec<usize> = (0..order.len())
        .filter(|&at| segment(record(order[at])).is_some())
        .collect();
    for boundary in boundaries.into_iter().rev() {
        let Some((head, tail)) = segment(record(order[boundary])) else {
            continue;
        };
        let after = |id: &MessageId| {
            order[boundary + 1..]
                .iter()
                .position(|side| record(*side).id() == id.as_str())
                .map(|at| boundary + 1 + at)
        };
        let (Some(head), Some(tail)) = (after(&head), after(&tail)) else {
            continue;
        };
        if tail < head {
            continue;
        }
        let moved: Vec<Side> = order.drain(head..=tail).collect();
        order.splice(boundary..boundary, moved);
    }
    order
}

/// 摘要上的压缩边界的保留段首尾（同冷读：用户消息、`info.summary` 为真时由 `hydrate_boundary` 换出边界）。
fn segment(record: &WithParts) -> Option<(MessageId, MessageId)> {
    if record.info["role"] != "user" || !crate::branch::truthy(record.info.get("summary")) {
        return None;
    }
    match crate::compaction::hydrate_boundary(&record.parts, record.id()) {
        Some(Message::System(SystemMessage::CompactBoundary(boundary))) => boundary
            .preserved
            .map(|preserved| (preserved.head, preserved.tail)),
        _ => None,
    }
}
