//! 压缩边界与摘要的 Node 记录：触发方式的名字与读回（docs/specs/rust-cli/compact.md §3.5、§5、§6），以及经 [`crate::rows`]
//! 的写入（node-store 与 pg-store 共用，pg-store.md §12）。
//!
//! 摘要之前另写 Node 压缩成功时的时间线记录（`persistCompactTimeline`，compact.md §6）：一条只给界面看的 assistant，带
//! `context_compaction` 的 timeline part 与带 `timelineStatus` 的 compaction part。Node 的 v4 冷读只从它合成压缩标记。
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    CompactBoundary, CompactTrigger, ContentBlock, Message, PreservedSegment, SystemMessage,
    UserMessage,
};
use rust_cli_kernel_api::store::StoreError;
use serde_json::{Value, json};

use crate::facts::{Commit, save_entry};
use crate::ids::{created_at, part_id};
use crate::keys::{self, EntryKey};
use crate::records::{self, AGENT, RUST_CLI};
use crate::{state, storage};

/// `compaction` part 的 `metadata.rustCli` 里记压缩前已加载的工具的键（compact.md §3.5）。
pub const DISCOVERED_KEY: &str = "preCompactDiscoveredTools";

/// trigger → Node 的 `trigger`、`phase`、`compactReason`。
pub fn node_trigger(trigger: CompactTrigger) -> (&'static str, &'static str, &'static str) {
    match trigger {
        CompactTrigger::Manual => ("manual", "standalone_turn", "user_requested"),
        CompactTrigger::Auto => ("auto", "pre_request", "context_limit"),
        CompactTrigger::Reactive => ("reactive", "reactive", "provider_overflow"),
    }
}

pub fn kernel_trigger(trigger: &str) -> CompactTrigger {
    match trigger {
        "manual" => CompactTrigger::Manual,
        "reactive" => CompactTrigger::Reactive,
        _ => CompactTrigger::Auto,
    }
}

/// 冷读取：摘要消息的 `compactBoundary` → 内核边界（摘要之前）。
pub fn hydrate_boundary(parts: &[Value], summary: &str) -> Option<Message> {
    let payload = parts
        .iter()
        .find(|part| part["type"] == "compaction" && part["compactBoundary"].is_object())
        .map(|part| &part["compactBoundary"])?;
    let preserved = payload["preservedSegment"].as_object().and_then(|segment| {
        let id = |key: &str| segment.get(key)?.as_str().map(MessageId::new);
        Some(PreservedSegment {
            head: id("headMessageId")?,
            anchor: id("anchorMessageId")?,
            tail: id("tailMessageId")?,
        })
    });
    let compaction = parts
        .iter()
        .find(|part| part["type"] == "compaction" && part["compactBoundary"].is_object());
    let discovered_tools = compaction
        .and_then(|part| part["metadata"][RUST_CLI][DISCOVERED_KEY].as_array())
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some(Message::System(SystemMessage::CompactBoundary(
        CompactBoundary {
            id: MessageId::new(payload["boundaryId"].as_str()?),
            trigger: kernel_trigger(payload["trigger"].as_str().unwrap_or("")),
            pre_tokens: payload["preCompactTokenCount"].as_u64().unwrap_or(0),
            summary: MessageId::new(payload["summaryMessageIds"][0].as_str().unwrap_or(summary)),
            preserved,
            discovered_tools,
        },
    )))
}

/// Node 压缩的 operationId 前缀（`compactTimeline.operationId = cmp_<uuid>`）。
const OPERATION_PREFIX: &str = "cmp_";
/// Node `CompactTimelineDisplay.Separator`。
const TIMELINE_DISPLAY: &str = "separator";
/// Node `CompactTimelineStatus.Completed`：rust 的压缩在一个事务里提交，只落成功的终态。
const TIMELINE_COMPLETED: &str = "completed";

pub fn append_boundary(ctx: &Commit<'_>, boundary: &CompactBoundary) -> Result<(), StoreError> {
    let data = serde_json::to_value(boundary).map_err(|error| storage(error.to_string()))?;
    let entry = records::entry(
        EntryKey::item(ctx.session, keys::COMPACT_BOUNDARY, boundary.id.as_str()).encode(),
        ctx.session,
        &keys::named(keys::COMPACT_BOUNDARY),
        ctx.now,
        data,
        false,
    );
    save_entry(ctx, &entry)
}

/// 同一事务里先到的边界（摘要 ID 对得上的最后一个）。
fn boundary_for(ctx: &Commit<'_>, summary: &str) -> Result<CompactBoundary, StoreError> {
    let entries = ctx
        .conn
        .entries(ctx.session, Some(&keys::named(keys::COMPACT_BOUNDARY)))?;
    entries
        .into_iter()
        .rev()
        .filter_map(|entry| serde_json::from_value::<CompactBoundary>(entry.data).ok())
        .find(|boundary| boundary.summary.as_str() == summary)
        .ok_or_else(|| {
            storage(format!(
                "compact summary {summary} has no boundary before it"
            ))
        })
}

/// 有 message 行的 ID：工具结果消息没有行（存在 assistant 的 tool part 里），换成它的 assistant。本工程写的结果按
/// `resultMessageId` 找；Node 写的结果与冷读取给 Node 的未结调用补在内存里的结果用 part id 作消息 id，按 part id 找。
/// 修 bug（工作线 F86）：以前只认 `resultMessageId`，保留段的首尾是这两种结果时提交报 `has no stored row`。
fn row_id(ctx: &Commit<'_>, id: &str) -> Result<String, StoreError> {
    if ctx.conn.message_exists(id)? {
        return Ok(id.to_owned());
    }
    ctx.conn
        .result_owner(ctx.session, id)?
        .ok_or_else(|| storage(format!("preserved message {id} has no stored row")))
}

/// `compactBoundary`（Node strict schema 的字段，键序同 schema）。`records` 是写入摘要之前会话的 message 行。
fn payload(
    ctx: &Commit<'_>,
    boundary: &CompactBoundary,
    summary: &str,
    records: &[crate::row::WithParts],
) -> Result<Value, StoreError> {
    let (trigger, phase, reason) = node_trigger(boundary.trigger);
    let rows: Vec<&str> = records.iter().map(crate::row::WithParts::id).collect();
    let segment = match &boundary.preserved {
        Some(segment) => {
            let head = row_id(ctx, segment.head.as_str())?;
            let tail = row_id(ctx, segment.tail.as_str())?;
            let position = |id: &str| rows.iter().position(|row| *row == id);
            let kept = match (position(&head), position(&tail)) {
                (Some(start), Some(end)) if end >= start => end - start + 1,
                _ => return Err(storage("preserved segment is not a stored range")),
            };
            Some((head, tail, kept))
        }
        None => None,
    };
    let kept = segment.as_ref().map_or(0, |(_, _, kept)| *kept);
    let row = session_row(ctx)?;
    let mut out = json!({
        "boundaryId": boundary.id.as_str(),
        "trigger": trigger,
        "phase": phase,
        "compactReason": reason,
        "summarySource": "model",
        "preCompactTokenCount": boundary.pre_tokens,
        "summarizedMessageCount": rows.len().saturating_sub(kept),
    });
    if let Some((head, tail, kept)) = segment {
        out["keptMessageCount"] = kept.into();
        out["preservedSegment"] =
            json!({"headMessageId": head, "anchorMessageId": summary, "tailMessageId": tail});
    }
    out["summaryMessageIds"] = json!([summary]);
    out["traceId"] = row
        .trace_id
        .unwrap_or_else(|| ctx.session.to_owned())
        .into();
    Ok(out)
}

fn session_row(ctx: &Commit<'_>) -> Result<crate::rows::SessionRow, StoreError> {
    ctx.conn
        .session(ctx.session)?
        .ok_or_else(|| storage(format!("session {} disappeared", ctx.session)))
}

/// 压缩时会话里最后一条对话消息（Node 的 `lastSummarizedMessageId`，即 `latestConversationMessageId`）：最后一行 user / assistant，
/// 跳过只给界面看的时间线消息与隐藏的 system reminder（Node 写它们时不移动这个指针）。
fn last_conversation(records: &[crate::row::WithParts]) -> Option<&str> {
    let conversation = |record: &&crate::row::WithParts| {
        let semantics = &record.info["semantics"];
        matches!(record.info["role"].as_str(), Some("user" | "assistant"))
            && semantics["providerVisibility"] != "hidden"
            && !(semantics["kind"] == "system_reminder" && semantics["uiVisibility"] == "hidden")
    };
    records
        .iter()
        .rev()
        .find(conversation)
        .map(crate::row::WithParts::id)
}

/// Node `persistCompactTimeline` 在压缩成功时留下的记录（compact.md §6）：一条只给界面看的 assistant（`crate::timeline`），
/// 带 `context_compaction` 的 timeline part（id `<P>_timeline`）与带 `timelineStatus` 的 compaction part（id `P`），两个 part 用同一个
/// `operation`。修 bug（R-RT-COMPACT-MARKER）：以前只写摘要，Node 的 v4 冷读不出压缩标记，rust 下压缩过的会话回到 Node 之后看不出压缩过。
fn timeline(
    ctx: &Commit<'_>,
    boundary: &CompactBoundary,
    (summary, operation, tail): (&str, &str, Option<&str>),
) -> Result<(), StoreError> {
    let row = session_row(ctx)?;
    let selection = state::selection(ctx.conn, ctx.session)?;
    let message = crate::timeline::save(ctx, &row.directory, selection.as_ref(), tail)?;
    let (trigger, phase, reason) = node_trigger(boundary.trigger);
    let now = ctx.now;
    let part = part_id(now, &(ctx.uuid)());
    let timeline = json!({"timelineType": "context_compaction", "display": TIMELINE_DISPLAY,
        "status": TIMELINE_COMPLETED, "operationId": operation, "trigger": trigger, "phase": phase,
        "compactReason": reason, "boundaryId": boundary.id.as_str(), "summaryMessageId": summary,
        "preCompactTokenCount": boundary.pre_tokens, "time": {"start": now, "end": now},
        "id": format!("{part}_timeline"), "sessionID": ctx.session, "messageID": message, "type": "timeline"});
    ctx.conn.save_part(&timeline, now as i64)?;
    let mut compaction = json!({"id": part, "sessionID": ctx.session, "messageID": message,
        "type": "compaction", "auto": trigger == "auto", "trigger": trigger, "phase": phase,
        "compactReason": reason, "operationId": operation, "timelineStatus": TIMELINE_COMPLETED,
        "timelineDisplay": TIMELINE_DISPLAY, "replace": true, "boundaryId": boundary.id.as_str(),
        "summaryMessageId": summary});
    if let Some(tail) = tail {
        compaction["tail_start_id"] = tail.into();
    }
    compaction["preCompactTokenCount"] = boundary.pre_tokens.into();
    compaction["time"] = json!({"start": now, "end": now});
    ctx.conn.save_part(&compaction, now as i64)?;
    Ok(())
}

/// Node `persistCompactSummary` 的消息与 part。
pub fn append_summary(ctx: &Commit<'_>, user: &UserMessage) -> Result<(), StoreError> {
    let text = match user.content.as_slice() {
        [ContentBlock::Text { text }] => text.as_str(),
        _ => {
            return Err(crate::facts::unsupported(
                "compact summary with non-text blocks",
            ));
        }
    };
    let id = user.id.as_str();
    let boundary = boundary_for(ctx, id)?;
    // 行数与最后一条对话消息都按写时间线记录之前的库算（Node 的这两项来自压缩时的运行时历史，不含这条记录）。
    let records = ctx.conn.messages(ctx.session)?;
    let compact_boundary = payload(ctx, &boundary, id, &records)?;
    let tail = last_conversation(&records);
    let operation = format!("{OPERATION_PREFIX}{}", (ctx.uuid)());
    timeline(ctx, &boundary, (id, &operation, tail))?;
    let created = created_at(id, ctx.now);
    let mut info = json!({"id": id, "sessionID": ctx.session, "role": "user",
        "time": {"created": created},
        "summary": {"title": "Compact summary", "body": text, "diffs": []},
        "agent": AGENT});
    if let Some(selection) = state::selection(ctx.conn, ctx.session)? {
        info["modelSelection"] =
            json!({"providerId": selection["providerId"], "modelId": selection["modelId"]});
    }
    info["semantics"] = json!({"origin": "agent_runtime", "kind": "compact_summary",
        "uiVisibility": "hidden", "providerVisibility": "visible", "transcriptVisibility": "hidden"});
    info["tools"] = json!({});
    ctx.conn.save_message(&info, ctx.now as i64)?;
    let text_part = json!({"id": part_id(ctx.now, &(ctx.uuid)()), "sessionID": ctx.session,
        "messageID": id, "type": "text", "text": text, "synthetic": true,
        "time": {"start": created, "end": created}});
    ctx.conn.save_part(&text_part, ctx.now as i64)?;
    let (trigger, phase, reason) = node_trigger(boundary.trigger);
    let mut compaction = json!({"id": part_id(ctx.now, &(ctx.uuid)()), "sessionID": ctx.session,
        "messageID": id, "type": "compaction", "auto": trigger == "auto", "trigger": trigger,
        "phase": phase, "compactReason": reason});
    // Node `persistCompactSummary` 的键序：tail_start_id、compactBoundary、operationId（v4 冷读按 operationId 把它与时间线记录合成一个标记）
    if let Some(tail) = tail {
        compaction["tail_start_id"] = tail.into();
    }
    compaction["compactBoundary"] = compact_boundary;
    compaction["operationId"] = operation.into();
    // 压缩前已加载的工具（compact.md §3.5）：`compactBoundary` 是 strict，放在 part 的 `metadata.rustCli`。
    if !boundary.discovered_tools.is_empty() {
        compaction["metadata"] = json!({ RUST_CLI: { DISCOVERED_KEY: boundary.discovered_tools } });
    }
    ctx.conn.save_part(&compaction, ctx.now as i64)?;
    Ok(())
}
