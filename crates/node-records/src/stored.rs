//! 要写的 message / part 行的列（Node `saveMessage` / `savePart`，`repositories/messages.ts`）：从带 id 的 Node 对象算出
//! 各列的值与 `data` 的存储形态。这是记录格式，本机库与云端库共用这一份；两个后端只负责把算好的列写进去（各自的
//! upsert 与“覆盖已有行时保留旧成员”的做法不同：SQLite 用 `json_set`，PG 读出旧行后用 [`preserve_legacy`]）。
//!
//! 以前这一段在两个后端里各写一份（`text`、`members`、`stored_part` 与两张 legacy 成员表逐字相同）。
use rust_cli_kernel_api::store::StoreError;
use serde_json::{Map, Value, json};

use crate::codecs::{legacy_user_model, part_created_at};
use crate::storage;

/// 覆盖同一会话里已有的 message 行时，旧行里有就保留的顶层成员（Node `preserveLegacyMembers`）。
pub const MESSAGE_LEGACY: [&str; 4] = ["model", "providerID", "modelID", "variant"];
/// 同上，part 行。
pub const PART_LEGACY: [&str; 3] = ["fromModel", "toModel", "model"];

/// 一条要写的 message 行。
#[derive(Debug, Clone, PartialEq)]
pub struct MessageRow<'a> {
    pub id: &'a str,
    pub session: &'a str,
    pub created: i64,
    /// assistant 是完成时刻（没有时是保存时刻），其余是创建时刻。
    pub updated: i64,
    /// `data` 列的对象：去掉 `id` 与 `sessionID`；user 消息补上旧版 Reader 无条件读取的 `model`。
    pub data: Map<String, Value>,
}

/// 一条要写的 part 行。
#[derive(Debug, Clone, PartialEq)]
pub struct PartRow<'a> {
    pub id: &'a str,
    pub message: &'a str,
    pub session: &'a str,
    pub created: i64,
    /// `data` 列的对象：去掉 `id`、`sessionID`、`messageID`；`model_change` 时间线与 subtask 换成存储形态。
    pub data: Map<String, Value>,
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, StoreError> {
    value[key]
        .as_str()
        .ok_or_else(|| storage(format!("Stored record needs a string {key}")))
}

fn members(value: &Value, strip: &[&str]) -> Result<Map<String, Value>, StoreError> {
    let Value::Object(map) = value else {
        return Err(storage("Stored record must be an object"));
    };
    let mut data = map.clone();
    for key in strip {
        data.shift_remove(*key);
    }
    Ok(data)
}

/// Node `saveMessage` 的列。`now` 是保存时的 `Date.now()`。
pub fn message_row(info: &Value, now: i64) -> Result<MessageRow<'_>, StoreError> {
    let id = text(info, "id")?;
    let session = text(info, "sessionID")?;
    let mut data = members(info, &["id", "sessionID"])?;
    if info["role"] == "user" {
        // 冻结旧版 Reader 无条件读取 user.model，缺少整个对象会让消息无法打开。
        data.insert(
            "model".into(),
            legacy_user_model(info.get("modelSelection")),
        );
    }
    let created = info["time"]["created"]
        .as_i64()
        .ok_or_else(|| storage("Stored message needs time.created"))?;
    let updated = if info["role"] == "assistant" {
        info["time"]["completed"].as_i64().unwrap_or(now)
    } else {
        created
    };
    Ok(MessageRow {
        id,
        session,
        created,
        updated,
        data,
    })
}

/// Node `savePart` 的列。`now` 是保存时的 `Date.now()`。
pub fn part_row(part: &Value, now: i64) -> Result<PartRow<'_>, StoreError> {
    Ok(PartRow {
        id: text(part, "id")?,
        message: text(part, "messageID")?,
        session: text(part, "sessionID")?,
        created: part_created_at(part, now),
        data: stored_part(part, members(part, &["id", "sessionID", "messageID"])?),
    })
}

/// Node `savePart` 对 `model_change` 时间线与 subtask 的存储形态。
fn stored_part(part: &Value, mut data: Map<String, Value>) -> Map<String, Value> {
    if part["type"] == "timeline" && part["timelineType"] == "model_change" {
        let from = data.shift_remove("fromModel");
        let to = data.shift_remove("toModel");
        let legacy = match to.as_ref().filter(|t| t.is_object()) {
            Some(to) => {
                let mut legacy = json!({"providerID": to["providerId"], "modelID": to["modelId"]});
                if let Some(level) = to["options"]["reasoningLevel"]
                    .as_str()
                    .filter(|l| !l.is_empty())
                {
                    legacy["variant"] = level.into();
                }
                if let Some(label) = to.get("label") {
                    legacy["label"] = label.clone();
                }
                legacy
            }
            None => json!({}),
        };
        data.insert("toModel".into(), legacy);
        // undefined 成员在 JSON.stringify 中省略。
        if let Some(from) = from {
            data.insert("fromModelSelection".into(), from);
        }
        if let Some(to) = to {
            data.insert("toModelSelection".into(), to);
        }
    } else if part["type"] == "subtask"
        && let Some(model) = data.shift_remove("model")
    {
        data.insert("modelSelection".into(), model);
    }
    data
}

/// Node `preserveLegacyMembers`：同一会话里覆盖已有行时，旧行里存在的 legacy 顶层成员写回新数据（已有的键原位替换，
/// 没有的追加在末尾，与 SQLite 的 `json_set` 相同）。旧行不是 JSON 对象时新数据原样。读出旧行再写的后端用它。
pub fn preserve_legacy(
    previous: &str,
    mut data: Map<String, Value>,
    keys: &[&str],
) -> Map<String, Value> {
    let Ok(Value::Object(previous)) = serde_json::from_str::<Value>(previous) else {
        return data;
    };
    for key in keys {
        if let Some(value) = previous.get(*key) {
            data.insert((*key).to_owned(), value.clone());
        }
    }
    data
}

#[cfg(test)]
#[path = "stored_tests.rs"]
mod tests;
