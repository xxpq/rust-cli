// 复制自 apps/zcode-cli-rust/crates/state/src/node/messages.rs（feat/rs 79b6691c55）。
//! `message` 与 `part` 行（Node `repositories/messages.ts`，docs/specs/rust-m11-node-storage.md
//! §2.3、§5）。值是带 id 成员的 Node `MessageInfo` / `MessagePart` 对象。
//!
//! 与复制来源的差异：去掉 fork 复制（`copyFrom`）与删除操作，本 crate 还不使用。
use super::codecs::decode_message;
use super::sessions::touch;
use crate::{
    entities::{message, part},
    orm::Db,
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use rust_cli_base::js::json::stringify;
use rust_cli_node_records::raw::{MessageHead, RawPart, RawRecord};
use rust_cli_node_records::stored::{self, MESSAGE_LEGACY, PART_LEGACY};
use sea_orm::sea_query::{Alias, Expr, ExprTrait};
use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect};
use serde_json::Value;
use std::sync::LazyLock;

/// Node `preserveLegacyMembers`：只把旧的顶层成员注回新数据。
fn preserve_legacy_members(table: &str, keys: &[&str]) -> String {
    keys.iter().fold("excluded.data".to_owned(), |sql, key| {
        format!(
            "(select case when json_type({table}.data, '$.{key}') is not null
      then json_set(previous.data, '$.{key}', json_extract({table}.data, '$.{key}')) else previous.data end
      from (select {sql} as data) as previous)"
        )
    })
}

static SAVE_MESSAGE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "insert into message (id, session_id, time_created, time_updated, data, sequence)
      values (?, ?, ?, ?, ?, (select coalesce(max(sequence), -1) + 1 from message where session_id = ?))
      on conflict(id) do update set
        session_id = excluded.session_id,
        time_updated = excluded.time_updated,
        data = case when message.session_id = excluded.session_id then {} else excluded.data end,
        sequence = case
          when message.session_id = excluded.session_id then message.sequence
          else excluded.sequence
        end",
        preserve_legacy_members("message", &MESSAGE_LEGACY)
    )
});

static SAVE_PART: LazyLock<String> = LazyLock::new(|| {
    format!(
        "insert into part (id, message_id, session_id, time_created, time_updated, data, sequence)
      values (?, ?, ?, ?, ?, ?, (select coalesce(max(sequence), -1) + 1 from part where message_id = ?))
      on conflict(id) do update set
        message_id = excluded.message_id,
        session_id = excluded.session_id,
        time_updated = excluded.time_updated,
        data = case when part.message_id = excluded.message_id and part.session_id = excluded.session_id
          then {} else excluded.data end,
        sequence = case
          when part.message_id = excluded.message_id and part.session_id = excluded.session_id
            then part.sequence
          else excluded.sequence
        end",
        preserve_legacy_members("part", &PART_LEGACY)
    )
});

/// Node `saveMessage`。`now` 是保存时的 `Date.now()`。各列的值与 `data` 的存储形态由 node-records 的 `stored` 算
/// （云端库用同一份）；这里只有 SQLite 的 upsert。
pub fn save_message(conn: &Connection, info: &Value, now: i64) -> Result<()> {
    let row = stored::message_row(info, now)?;
    conn.prepare_cached(&SAVE_MESSAGE)?.execute(params![
        row.id,
        row.session,
        row.created,
        row.updated,
        stringify(&Value::Object(row.data)),
        row.session
    ])?;
    touch(conn, row.session, row.updated)?;
    Ok(())
}

/// Node `savePart`。`now` 是保存时的 `Date.now()`。
pub fn save_part(conn: &Connection, part: &Value, now: i64) -> Result<()> {
    let row = stored::part_row(part, now)?;
    conn.prepare_cached(&SAVE_PART)?.execute(params![
        row.id,
        row.message,
        row.session,
        row.created,
        now,
        stringify(&Value::Object(row.data)),
        row.message
    ])?;
    touch(conn, row.session, now)?;
    Ok(())
}

/// 这个会话的 part，按 Node `messages` 的行序：按消息分组，组内 `sequence`（NULL 在后）、创建时间、id。[`messages_raw`] 与 [`tool_parts`] 共用。
fn ordered_parts(session: &str) -> sea_orm::Select<part::Entity> {
    part::Entity::find()
        .filter(part::Column::SessionId.eq(session))
        .order_by_asc(part::Column::MessageId)
        .order_by(Expr::col(part::Column::Sequence).is_null(), Order::Asc)
        .order_by_asc(part::Column::Sequence)
        .order_by_asc(part::Column::TimeCreated)
        .order_by_asc(part::Column::Id)
}

/// data 里的一个成员；data 不是合法 JSON 时为 NULL（同 `tool_part` 的写法）。
fn data_member(key: &str) -> Expr {
    Expr::cust(format!(
        "(case when json_valid(data) then json_extract(data, '$.{key}') end)"
    ))
}

/// 消息的行序（Node `messages`）：`sequence`（NULL 在后）、创建时间、rowid。
fn ordered_messages(session: &str) -> sea_orm::Select<message::Entity> {
    message::Entity::find()
        .filter(message::Column::SessionId.eq(session))
        .order_by(Expr::col(message::Column::Sequence).is_null(), Order::Asc)
        .order_by_asc(message::Column::Sequence)
        .order_by_asc(message::Column::TimeCreated)
        .order_by_asc(Expr::col(Alias::new("rowid")))
}

/// Node `messages`：按 `sequence` 排序（NULL 在后），part 按消息分组；part 只留 data 原文，用到时再解码（node-store.md §5.4）。
pub fn messages_raw(conn: &Connection, session: &str) -> Result<Vec<RawRecord>> {
    raw(
        conn,
        session,
        ordered_parts(session),
        ordered_messages(session),
    )
}

/// 会话里给出的这些消息与它们的 part（按 id，行序同 [`messages_raw`]；v5 的分页按 id 读历史的一段，node-store.md §5.5）。
pub fn messages_raw_of(conn: &Connection, session: &str, ids: &[&str]) -> Result<Vec<RawRecord>> {
    raw(
        conn,
        session,
        ordered_parts(session).filter(part::Column::MessageId.is_in(ids.iter().copied())),
        ordered_messages(session).filter(message::Column::Id.is_in(ids.iter().copied())),
    )
}

/// 按两个查询读消息行与 part 行，part 按消息分组、只留 data 原文。
fn raw(
    conn: &Connection,
    session: &str,
    parts_query: sea_orm::Select<part::Entity>,
    messages_query: sea_orm::Select<message::Entity>,
) -> Result<Vec<RawRecord>> {
    let mut parts: std::collections::HashMap<String, Vec<RawPart>> = Default::default();
    for row in parts_query.all(&Db(conn))? {
        parts.entry(row.message_id).or_default().push(RawPart {
            id: row.id,
            data: row.data,
        });
    }
    messages_query
        .all(&Db(conn))?
        .into_iter()
        .map(|row| {
            Ok(RawRecord {
                info: decode_message(&row.data, &row.id, &row.session_id)?,
                session: session.to_owned(),
                parts: parts.remove(&row.id).unwrap_or_default(),
            })
        })
        .collect()
}

/// 会话的全部消息行的头（id、role、summary），按 [`messages_raw`] 的消息行序；不读 part、不建整份 info（范围读的历史索引，node-store.md §5.6、§5.7）。
///
/// 修性能缺陷（PERF-RUST-A，node-store.md §5.7）：以前经 ORM 按 `sequence is null, sequence, time_created, rowid` 在 SQLite 里排，排序键用不上
/// `message_session_sequence_idx`，整行（data 列）进临时 B 树，大会话上落盘；再整份解码成 info。现在只按会话取行，在这里按同一个键排，data 只借用解析两个成员
pub fn message_heads(conn: &Connection, session: &str) -> Result<Vec<MessageHead>> {
    /// [`ordered_messages`] 的排序键：`sequence` 是 NULL、`sequence`、`time_created`、`rowid`。
    type Order = (bool, i64, i64, i64);
    let mut statement = conn.prepare_cached(
        "select id, data, sequence, time_created, rowid from message where session_id = ?1",
    )?;
    let mut rows: Vec<(Order, String, String)> = statement
        .query_map([session], |row| {
            let sequence: Option<i64> = row.get(2)?;
            let key = (
                sequence.is_none(),
                sequence.unwrap_or_default(),
                row.get(3)?,
                row.get(4)?,
            );
            Ok((key, row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    rows.sort_unstable_by_key(|(key, _, _)| *key);
    rows.into_iter()
        .map(|(_, id, data)| Ok(MessageHead::parse(id, &data)?))
        .collect()
}

/// 会话的全部消息的 id，按 [`messages_raw`] 的消息行序（子代理的冷读只按 id 选分支）。
pub fn message_ids(conn: &Connection, session: &str) -> Result<Vec<String>> {
    Ok(ordered_messages(session)
        .select_only()
        .column(message::Column::Id)
        .into_tuple()
        .all(&Db(conn))?)
}

/// `type = tool`、`tool` 在 `tools` 里的 part（原文）与它所在消息的 id，按 [`ordered_parts`] 的行序；data 不是合法 JSON 的不选。
pub fn tool_parts(
    conn: &Connection,
    session: &str,
    tools: &[&str],
) -> Result<Vec<(String, RawPart)>> {
    Ok(ordered_parts(session)
        .filter(data_member("type").eq("tool"))
        .filter(data_member("tool").is_in(tools.iter().copied()))
        .all(&Db(conn))?
        .into_iter()
        .map(|row| {
            let part = RawPart {
                id: row.id,
                data: row.data,
            };
            (row.message_id, part)
        })
        .collect())
}

/// 消息 ID 是旧库全局主键。
pub fn exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(message::Entity::find_by_id(id).one(&Db(conn))?.is_some())
}

/// 首条真实用户输入的创建时间：沿 `message_session_time_created_id_idx` 的顺序找第一条 `role = user`、
/// `semantics.origin = real_user` 的消息（同 [`FIRST_PROMPT`] 的消息）。
const FIRST_INPUT_AT: &str = "select time_created from message
      where session_id = ?1
        and (case when json_valid(data) then json_extract(data, '$.role') end) = 'user'
        and (case when json_valid(data) then json_extract(data, '$.semantics.origin') end) = 'real_user'
      order by time_created, id
      limit 1";

/// 首条真实用户输入的创建时间（[`FIRST_INPUT_AT`]）。
pub fn first_input_at(conn: &Connection, session: &str) -> Result<Option<i64>> {
    Ok(conn
        .prepare_cached(FIRST_INPUT_AT)?
        .query_row([session], |row| row.get(0))
        .optional()?)
}

/// 会话的第一条消息（`message_session_time_created_id_idx` 的顺序）的 id 与 data 列。
pub fn first_message(conn: &Connection, session: &str) -> Result<Option<(String, String)>> {
    Ok(conn
        .prepare_cached(
            "select id, data from message where session_id = ? order by time_created, id limit 1",
        )?
        .query_row([session], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?)
}

/// 这条消息的 part 里有没有 `metadata.<key> = value` 的。
pub fn part_metadata_is(conn: &Connection, message: &str, key: &str, value: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "select 1 from part where message_id = ?1
              and (case when json_valid(data) then json_extract(data, '$.metadata.' || ?2) end) = ?3
              limit 1",
        )?
        .query_row(params![message, key, value], |_| Ok(()))
        .optional()?
        .is_some())
}
