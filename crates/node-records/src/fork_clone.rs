//! Node `/fork` 的行克隆与目标状态（docs/specs/rust-cli/session-lifecycle.md §6.5.2 的映射表）。
//!
//! 按 Node legacy fork 的规则（`helpers/steering.ts` 的 `cloneMessageForFork` / `clonePartForFork`，只给出消息 ID
//! 映射；`session-fork.ts` 的 `copyGoalStateForFork`）：映射按复制顺序逐条加入，所以指向后面消息的引用保持原值。
//! 对象成员的位置按 JS 展开：已有的成员原地换值，新成员追加在后，删掉的成员不留位置。
use std::collections::{HashMap, HashSet};

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::input_extras::InputRecord;
use rust_cli_kernel_api::plan::ShownPlan;
use rust_cli_kernel_api::store::{StoreError, TurnRecord, entry_kinds};
use serde_json::{Map, Value, json};

use crate::facts::Commit;
use crate::ids::{message_id, part_id};
use crate::records::RUST_CLI;
use crate::row::WithParts;
use crate::rows::Entry;
use crate::rows::TargetRow;

/// 父会话的消息 ID → 新会话的消息 ID。
pub type Mapping = HashMap<String, String>;

const VERIFICATION_ENTRY: &str = "target_completion_verification";
/// 工具调用的 part 与它的调用 id（Node 的 tool part）。
const TOOL_PART: &str = "tool";
const CALL_ID: &str = "callID";

pub fn object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// 能映射就换成新 ID，否则保持原值（legacy，`strictLocalReferences` 为假）。
pub fn mapped(map: &Mapping, value: &Value) -> Value {
    value
        .as_str()
        .and_then(|id| map.get(id))
        .map_or_else(|| value.clone(), |id| Value::from(id.as_str()))
}

pub fn mapped_list(map: &Mapping, value: &Value) -> Value {
    match value.as_array() {
        Some(ids) => Value::Array(ids.iter().map(|id| mapped(map, id)).collect()),
        None => value.clone(),
    }
}

pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null | Value::Bool(false)) => false,
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

/// Node `cloneMessageAnchorForFork`（legacy）。
pub fn anchor(anchor: &Value, map: &Mapping, child: &str) -> Value {
    let mut out = object(anchor);
    if truthy(anchor.get("orderedMessageIds")) {
        out.insert(
            "orderedMessageIds".into(),
            mapped_list(map, &anchor["orderedMessageIds"]),
        );
    }
    if truthy(anchor.get("boundaryMessageId")) {
        out.insert(
            "boundaryMessageId".into(),
            mapped(map, &anchor["boundaryMessageId"]),
        );
    }
    let boundary = &anchor["goalBoundary"];
    if boundary["kind"] == "snapshot" {
        let mut target = object(&boundary["target"]);
        target.insert("sessionID".into(), child.into());
        for key in [
            "activeInputId",
            "activeRunStartedAtMs",
            "activeRunLastSeenAtMs",
        ] {
            target.insert(key.into(), Value::Null);
        }
        out.insert(
            "goalBoundary".into(),
            json!({"kind": "snapshot", "target": target,
                "verificationEntryIds": boundary["verificationEntryIds"]}),
        );
    }
    Value::Object(out)
}

/// Node `cloneMessageForFork`（legacy）。
pub fn message(info: &Value, map: &Mapping, child: &str, next: &str) -> Value {
    let mut out = object(info);
    let origin = json!({"sessionId": info["sessionID"], "messageId": info["id"]});
    out.insert("id".into(), next.into());
    out.insert("sessionID".into(), child.into());
    if info["role"] != "user" {
        out.insert("parentID".into(), mapped(map, &info["parentID"]));
    }
    if let Some(value) = info.get("anchor").filter(|value| truthy(Some(value))) {
        out.insert("anchor".into(), anchor(value, map, child));
    }
    let mut metadata = object(&info["metadata"]);
    metadata.insert("forkOrigin".into(), origin);
    out.insert("metadata".into(), Value::Object(metadata));
    Value::Object(out)
}

/// `compactBoundary` 的消息 ID 映射（Node `clonePartForFork` 的 compaction 分支）。
pub fn boundary(boundary: &Value, map: &Mapping) -> Value {
    let mut out = object(boundary);
    if truthy(boundary.get("lastSummarizedMessageId")) {
        out.insert(
            "lastSummarizedMessageId".into(),
            mapped(map, &boundary["lastSummarizedMessageId"]),
        );
    }
    out.insert(
        "summaryMessageIds".into(),
        mapped_list(map, &boundary["summaryMessageIds"]),
    );
    for key in ["attachmentMessageIds", "hookResultMessageIds"] {
        if truthy(boundary.get(key)) {
            out.insert(key.into(), mapped_list(map, &boundary[key]));
        }
    }
    if let Some(segment) = boundary.get("preservedSegment").filter(|v| truthy(Some(v))) {
        out.insert(
            "preservedSegment".into(),
            json!({
                "headMessageId": mapped(map, &segment["headMessageId"]),
                "anchorMessageId": mapped(map, &segment["anchorMessageId"]),
                "tailMessageId": mapped(map, &segment["tailMessageId"]),
            }),
        );
    }
    Value::Object(out)
}

/// Node `clonePartForFork`（legacy：只有消息 ID 映射）。
pub fn part(ctx: &Commit<'_>, part: &Value, map: &Mapping, next: &str) -> Value {
    let new_id = || part_id(ctx.now, &(ctx.uuid)());
    let mut out = object(part);
    out.insert("id".into(), new_id().into());
    out.insert("sessionID".into(), ctx.session.into());
    out.insert("messageID".into(), next.into());
    match part["type"].as_str() {
        Some("timeline") => {
            if let Some(anchor) = part.get("anchorMessageId").filter(|v| truthy(Some(v))) {
                match anchor.as_str().and_then(|id| map.get(id)) {
                    Some(id) => {
                        out.insert("anchorMessageId".into(), id.as_str().into());
                    }
                    None => {
                        out.shift_remove("anchorMessageId");
                        out.insert("originAnchorMessageId".into(), anchor.clone());
                    }
                }
            }
            // legacy fork 没有轮次映射：只能降级成来源记录。
            if let Some(turn) = part.get("anchorTurnId").filter(|v| truthy(Some(v))) {
                out.shift_remove("anchorTurnId");
                out.insert("originAnchorTurnId".into(), turn.clone());
            }
            if part["timelineType"] == "context_compaction" && truthy(part.get("summaryMessageId"))
            {
                out.insert(
                    "summaryMessageId".into(),
                    mapped(map, &part["summaryMessageId"]),
                );
            }
        }
        Some("compaction") => {
            for key in ["summaryMessageId", "tail_start_id"] {
                if truthy(part.get(key)) {
                    out.insert(key.into(), mapped(map, &part[key]));
                }
            }
            if let Some(value) = part.get("compactBoundary").filter(|v| truthy(Some(v))) {
                out.insert("compactBoundary".into(), boundary(value, map));
            }
        }
        Some("tool") if part["state"]["status"] == "completed" => {
            if let Some(attachments) = part["state"]["attachments"].as_array() {
                let mut state = object(&part["state"]);
                let cloned = attachments
                    .iter()
                    .map(|attachment| {
                        let mut copy = object(attachment);
                        copy.insert("id".into(), new_id().into());
                        copy.insert("sessionID".into(), ctx.session.into());
                        copy.insert("messageID".into(), next.into());
                        Value::Object(copy)
                    })
                    .collect();
                state.insert("attachments".into(), Value::Array(cloned));
                out.insert("state".into(), Value::Object(state));
            }
        }
        _ => {}
    }
    Value::Object(out)
}

/// 复制的消息在新会话里给谁看。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Copied {
    /// fork：照原样。
    AsIs,
    /// 侧聊继承的历史（session-lifecycle.md §10.58）：只给模型看。
    ModelOnly,
}

/// Node `commitAtomicConversationFork` 的 selection_side_chat 分支：`visibility` 与 `semantics` 换成只给模型看（已有的成员原地换值、没有的追加），
/// 去掉 `anchor.goalBoundary`（Node 在克隆之前去掉：侧聊不复制目标，留着它冷读会要一个不存在的目标）。
fn model_only(info: &mut Value) {
    let semantics = &info["semantics"];
    // JS 的 `??`：缺省或 null 时用缺省值
    let or = |key: &str, fallback: &str| {
        semantics
            .get(key)
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| fallback.into())
    };
    let mut hidden = Map::new();
    hidden.insert("origin".into(), or("origin", "migration"));
    hidden.insert("kind".into(), or("kind", "system_reminder"));
    if let Some(source) = semantics.get("source").filter(|v| truthy(Some(v))) {
        hidden.insert("source".into(), source.clone());
    }
    hidden.insert("uiVisibility".into(), "hidden".into());
    hidden.insert("providerVisibility".into(), "visible".into());
    hidden.insert("transcriptVisibility".into(), "hidden".into());
    info["visibility"] = "model-only".into();
    info["semantics"] = Value::Object(hidden);
    if let Some(anchor) = info.get_mut("anchor").and_then(Value::as_object_mut) {
        anchor.shift_remove("goalBoundary");
    }
}

/// Node `copySessionMessagesForFork`：逐条换新 ID 写进新会话，返回消息 ID 映射。
pub fn copy_messages(
    ctx: &Commit<'_>,
    records: &[&WithParts],
    copied: Copied,
) -> Result<Mapping, StoreError> {
    let mut map = Mapping::new();
    // 带位置键的 tool part（node-store.md §4.6，工作线 F81）：键指向 assistant 之后的行，逐条映射时还没有新 ID。
    let mut keyed = Vec::new();
    for record in records {
        let next = message_id(ctx.now, &(ctx.uuid)());
        map.insert(record.id().to_owned(), next.clone());
        let mut info = message(&record.info, &map, ctx.session, &next);
        if copied == Copied::ModelOnly {
            model_only(&mut info);
        }
        ctx.conn.save_message(&info, ctx.now as i64)?;
        for source in &record.parts {
            let cloned = part(ctx, source, &map, &next);
            ctx.conn.save_part(&cloned, ctx.now as i64)?;
            if cloned["metadata"][RUST_CLI]["afterMessageId"].is_string() {
                keyed.push(cloned);
            }
        }
    }
    // 修 bug（工作线 F81）：位置键不换时还指向原会话的行，分出的会话冷读取找不到，结果又都排回 assistant 之后。复制完
    // 之后按整张映射换成新 ID，再存一次这个 part。
    for mut cloned in keyed {
        let key = &mut cloned["metadata"][RUST_CLI]["afterMessageId"];
        if let Some(next) = key.as_str().and_then(|id| map.get(id)) {
            *key = Value::from(next.as_str());
            ctx.conn.save_part(&cloned, ctx.now as i64)?;
        }
    }
    Ok(map)
}

/// Node `cloneGoalVerificationEntryForFork`：锚定的 assistant 不在复制范围里的跳过。
pub fn verification(
    ctx: &Commit<'_>,
    entry: &Entry,
    map: &Mapping,
    target: &str,
) -> Option<(Entry, Value)> {
    let payload = &entry.data["payload"];
    if payload["targetId"] != target {
        return None;
    }
    let anchor = payload["anchorAssistantMessageId"].as_str();
    if anchor.is_some_and(|id| !map.contains_key(id)) {
        return None;
    }
    let mut cloned = object(payload);
    if let Some(id) = anchor.and_then(|id| map.get(id)) {
        cloned.insert("anchorAssistantMessageId".into(), id.as_str().into());
    }
    if let Some(turn) = payload["anchorTurnId"].as_str() {
        cloned.shift_remove("anchorTurnId");
        cloned.insert("originAnchorTurnId".into(), turn.into());
    }
    let event = (ctx.uuid)();
    let mut data = object(&entry.data);
    data.insert("eventId".into(), event.as_str().into());
    data.insert("payload".into(), Value::Object(cloned.clone()));
    let copy = Entry {
        id: format!("fork_goal_verify_{event}"),
        session_id: ctx.session.to_owned(),
        data: Value::Object(data),
        touch_session: true,
        ..entry.clone()
    };
    Some((copy, Value::Object(cloned)))
}

/// Node `deriveForkedGoalStatusFromCopiedVerifications`。
pub fn forked_status(parent: &str, copied: &[Value]) -> String {
    let latest = copied
        .iter()
        .rev()
        .find(|payload| payload["status"] == "completed" && truthy(payload.get("verification")));
    if latest.is_some_and(|payload| payload["verification"]["passed"] == true) {
        return "complete".to_owned();
    }
    if parent == "complete" {
        return "active".to_owned();
    }
    parent.to_owned()
}

/// Node `copyGoalStateForFork`（legacy，没有目标边界）：复制验证记录，再按 `cloneSessionTargetForFork` 写
/// `session_target`。原会话没有目标时什么都不写。
/// 本工程的回合记录与输入记录（docs/specs/rust-cli/session-lifecycle.md §10.18）：`first` 或键是复制了的消息的，按映射换成新的
/// 消息 id 写进新会话（回合 id、结局、取消的工具不变：tool_use id 复制时也不变）；指向没复制的消息的不写。ExitPlanMode 的计划（§10.57）：
/// 键是调用 id，复制了的消息里的调用（tool part 的 `callID`）的原样写进新会话。
///
/// 修 bug（02 §12.7 审定）：以前分叉不复制回合记录，新会话里那些消息不属于任何回合，按回合分组的行散开。
/// 修 bug（§10.57）：以前也不复制计划，新会话冷读时 ExitPlanMode 行没有计划文字（计划文件之后会被改写，冷读只能读这一份）。
pub fn copy_records(
    ctx: &Commit<'_>,
    parent: &str,
    map: &Mapping,
    copied: &[&WithParts],
) -> Result<(), StoreError> {
    for (id, data) in crate::rust_entries::kind_entries(ctx.conn, parent, entry_kinds::TURN)? {
        let Some(record) = TurnRecord::from_entry(&id, &data) else {
            continue;
        };
        if let Some(first) = map.get(record.first.as_str()) {
            let record = TurnRecord {
                first: MessageId::new(first),
                ..record
            };
            crate::facts::apply(ctx, &record.fact())?;
        }
    }
    for (id, data) in crate::rust_entries::kind_entries(ctx.conn, parent, entry_kinds::INPUT)? {
        let Some(record) = InputRecord::from_entry(&id, &data) else {
            continue;
        };
        if let Some(message) = map.get(record.message.as_str()) {
            let record = InputRecord {
                message: MessageId::new(message),
                ..record
            };
            crate::facts::apply(ctx, &record.fact())?;
        }
    }
    let calls: HashSet<&str> = copied
        .iter()
        .flat_map(|record| &record.parts)
        .filter(|part| part["type"] == TOOL_PART)
        .filter_map(|part| part[CALL_ID].as_str())
        .collect();
    for (id, data) in crate::rust_entries::kind_entries(ctx.conn, parent, entry_kinds::PLAN)? {
        if calls.contains(id.as_str())
            && let Some(plan) = ShownPlan::from_entry(&id, &data)
        {
            crate::facts::apply(ctx, &plan.fact())?;
        }
    }
    Ok(())
}

pub fn copy_goal(ctx: &Commit<'_>, parent: &str, map: &Mapping) -> Result<(), StoreError> {
    let Some(row) = ctx.conn.session_target(parent)? else {
        return Ok(());
    };
    let target = row.target_id.clone();
    let mut copied = Vec::new();
    for entry in ctx.conn.entries(parent, Some(VERIFICATION_ENTRY))? {
        if let Some((copy, payload)) = verification(ctx, &entry, map, &target) {
            ctx.conn.save_entry(&copy)?;
            copied.push(payload);
        }
    }
    let status = forked_status(&row.status, &copied);
    ctx.conn.insert_session_target(&TargetRow {
        session_id: ctx.session.into(),
        status,
        ..row
    })?;
    ctx.conn.touch_session(ctx.session, ctx.now as i64)?;
    Ok(())
}
