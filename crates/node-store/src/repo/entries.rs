// 复制自 apps/zcode-cli-rust/crates/state/src/node/entries.rs（feat/rs 79b6691c55）。
//! `session_entry` 行（Node `repositories/session-entries.ts`，docs/specs/rust-m11-node-storage.md §4）。
use super::codecs::{MODEL_SELECTION_ENTRY, decode_entry_data};
use super::sessions::touch;
use crate::{entities::session_entry, orm::Db};
use anyhow::Result;
use rusqlite::{Connection, params};
use rust_cli_base::js::json::stringify;
use sea_orm::sea_query::{Alias, Expr, OnConflict};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde_json::json;

pub use rust_cli_node_records::rows::Entry;

/// Node `saveSessionEntry`：保留 `time_created` 的 upsert；模型选择只替换 `$.modelSelection`，
/// 保留 legacy 字段。
pub fn save(conn: &Connection, entry: &Entry) -> Result<()> {
    let selection = entry.kind == MODEL_SELECTION_ENTRY;
    let data = if selection {
        json!({"modelSelection": entry.data})
    } else {
        entry.data.clone()
    };
    anyhow::ensure!(
        selection || !data.is_null(),
        "Session entry data must be JSON-serializable"
    );
    if selection {
        // 旧模型记录只替换一个 JSON 成员，保留冻结版本仍会读取的字段。
        conn.prepare_cached(
        "insert into session_entry (id, session_id, type, time_created, time_updated, data)
      values (?, ?, ?, ?, ?, ?)
      on conflict(id) do update set
        session_id = excluded.session_id,
        type = excluded.type,
        time_updated = excluded.time_updated,
        data = case
          when ? and session_entry.type = excluded.type
            and session_entry.session_id = excluded.session_id
            and json_type(session_entry.data) = 'object'
          then json_set(session_entry.data, '$.modelSelection', json_extract(excluded.data, '$.modelSelection'))
          else excluded.data
        end",
    )?
    .execute(params![
        entry.id,
        entry.session_id,
        entry.kind,
        entry.time_created,
        entry.time_updated,
        stringify(&data),
        i64::from(selection)
    ])?;
    } else {
        session_entry::Entity::insert(session_entry::ActiveModel {
            id: Set(entry.id.clone()),
            session_id: Set(entry.session_id.clone()),
            kind: Set(entry.kind.clone()),
            time_created: Set(entry.time_created),
            time_updated: Set(entry.time_updated),
            data: Set(stringify(&data)),
        })
        .on_conflict(
            OnConflict::column(session_entry::Column::Id)
                .update_columns([
                    session_entry::Column::SessionId,
                    session_entry::Column::Kind,
                    session_entry::Column::TimeUpdated,
                    session_entry::Column::Data,
                ])
                .to_owned(),
        )
        .exec(&Db(conn))?;
    }
    if entry.touch_session {
        touch(conn, &entry.session_id, entry.time_updated)?;
    }
    Ok(())
}

/// Node `sessionEntries`（`order by time_created, rowid`）；`kind` 按 type 过滤。
pub fn list(conn: &Connection, session: &str, kind: Option<&str>) -> Result<Vec<Entry>> {
    let mut query = session_entry::Entity::find()
        .filter(session_entry::Column::SessionId.eq(session))
        .order_by_asc(session_entry::Column::TimeCreated)
        .order_by_asc(Expr::col(Alias::new("rowid")));
    if let Some(kind) = kind {
        query = query.filter(session_entry::Column::Kind.eq(kind));
    }
    query
        .all(&Db(conn))?
        .into_iter()
        .map(|row| {
            Ok(Entry {
                data: decode_entry_data(&row.kind, &row.data)?,
                id: row.id,
                session_id: row.session_id,
                kind: row.kind,
                time_created: row.time_created,
                time_updated: row.time_updated,
                touch_session: true,
            })
        })
        .collect()
}
