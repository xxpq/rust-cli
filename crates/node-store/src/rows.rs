//! [`Rows`] 的本机实现：Node CLI 的 `db.sqlite`（rusqlite 连接或事务，`Sqlite(&tx)`）。
//! 编排在 node-records；这里只有行操作。原生 SQL 都是从编排模块原样移来的（JSON 路径与 rowid 排序是 SQLite 的写法）。
use rusqlite::{Connection, OptionalExtension, params};
use rust_cli_node_records::raw::{MessageHead, RawPart, RawRecord};
use rust_cli_node_records::rows::{
    Create, Entry, ForkSource, ModelRow, PartRow, Result, Rows, SessionRow, TargetRow, ToolRow,
    TurnRow, TurnTotals, Update,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::Value;

use crate::entities::{local_setting, message, permission, session, session_entry, session_target};
use crate::orm::Db;
use crate::repo;

/// 行操作的错误在这一处换成存储错误：本机库的失败（rusqlite、ORM、JSON）都是 `StoreError::Storage`，文字是原错误的
/// 文字。方法体里照旧用 `?`（`anyhow` 只在这个文件里当载体）。
fn stored<T>(work: impl FnOnce() -> anyhow::Result<T>) -> Result<T> {
    work().map_err(crate::storage)
}

/// 一个 rusqlite 连接（或事务）上的行操作。
pub(crate) struct Sqlite<'a>(pub(crate) &'a Connection);

impl Rows for Sqlite<'_> {
    fn session(&self, id: &str) -> Result<Option<SessionRow>> {
        stored(|| repo::sessions::get(self.0, id))
    }

    fn fork_source(&self, id: &str) -> Result<Option<ForkSource>> {
        stored(|| {
            Ok(session::Entity::find_by_id(id)
                .filter(session::Column::TimeDeleted.is_null())
                .one(&Db(self.0))?
                .map(|row| ForkSource {
                    project_id: row.project_id,
                    workspace_id: row.workspace_id,
                    trace_id: row.trace_id,
                    slug: row.slug,
                    directory: row.directory,
                    path: row.path,
                    title: row.title,
                    version: row.version,
                    permission: row.permission,
                    revert: row.revert,
                }))
        })
    }

    fn session_project_id(&self, id: &str) -> Result<Option<String>> {
        stored(|| {
            Ok(session::Entity::find_by_id(id)
                .one(&Db(self.0))?
                .map(|row| row.project_id))
        })
    }

    fn create_session(&self, input: &Create, now: i64) -> Result<()> {
        stored(|| repo::sessions::create(self.0, input, now))
    }

    fn update_session(&self, input: &Update, now: i64) -> Result<usize> {
        stored(|| repo::sessions::update(self.0, input, now))
    }

    fn touch_session(&self, id: &str, time_updated: i64) -> Result<()> {
        stored(|| repo::sessions::touch(self.0, id, time_updated))
    }

    fn soft_delete_session(&self, id: &str, time_deleted: i64) -> Result<()> {
        stored(|| repo::sessions::soft_delete(self.0, id, time_deleted))
    }

    fn list_sessions(&self, directory: &str, limit: Option<i64>) -> Result<Vec<SessionRow>> {
        stored(|| repo::sessions::list(self.0, directory, limit))
    }

    fn query_sessions(
        &self,
        directory: Option<&str>,
        created: bool,
        limit: Option<i64>,
    ) -> Result<Vec<SessionRow>> {
        stored(|| repo::sessions::query(self.0, directory, created, limit))
    }

    fn first_prompt_part(&self, session: &str) -> Result<Option<String>> {
        stored(|| {
            Ok(self
                .0
                .prepare_cached(crate::query::FIRST_PROMPT)?
                .query_row([session], |row| row.get(0))
                .optional()?)
        })
    }

    fn sessions_by_ids(&self, ids: &[&str]) -> Result<Vec<SessionRow>> {
        stored(|| repo::sessions::by_ids(self.0, ids))
    }

    fn first_input_at(&self, session: &str) -> Result<Option<i64>> {
        stored(|| repo::messages::first_input_at(self.0, session))
    }

    fn first_message(&self, session: &str) -> Result<Option<(String, String)>> {
        stored(|| repo::messages::first_message(self.0, session))
    }

    fn part_metadata_is(&self, message: &str, key: &str, value: &str) -> Result<bool> {
        stored(|| repo::messages::part_metadata_is(self.0, message, key, value))
    }

    fn save_message(&self, info: &Value, now: i64) -> Result<()> {
        stored(|| repo::messages::save_message(self.0, info, now))
    }

    fn save_part(&self, part: &Value, now: i64) -> Result<()> {
        stored(|| repo::messages::save_part(self.0, part, now))
    }

    fn messages_raw(&self, session: &str) -> Result<Vec<RawRecord>> {
        stored(|| repo::messages::messages_raw(self.0, session))
    }

    fn messages_raw_of(&self, session: &str, ids: &[&str]) -> Result<Vec<RawRecord>> {
        stored(|| repo::messages::messages_raw_of(self.0, session, ids))
    }

    fn message_ids(&self, session: &str) -> Result<Vec<String>> {
        stored(|| repo::messages::message_ids(self.0, session))
    }

    fn message_heads(&self, session: &str) -> Result<Vec<MessageHead>> {
        stored(|| repo::messages::message_heads(self.0, session))
    }

    fn tool_parts(&self, session: &str, tools: &[&str]) -> Result<Vec<(String, RawPart)>> {
        stored(|| repo::messages::tool_parts(self.0, session, tools))
    }

    fn message_exists(&self, id: &str) -> Result<bool> {
        stored(|| repo::messages::exists(self.0, id))
    }

    fn message_data(&self, session: &str, id: &str) -> Result<Option<String>> {
        stored(|| {
            Ok(message::Entity::find_by_id(id)
                .filter(message::Column::SessionId.eq(session))
                .one(&Db(self.0))?
                .map(|row| row.data))
        })
    }

    fn current_user_row(&self, session: &str) -> Result<Option<(String, String)>> {
        stored(|| {
            for sql in crate::state::CURRENT_USER_QUERIES {
                let stored = self
                    .0
                    .prepare_cached(sql)?
                    .query_row([session], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                if stored.is_some() {
                    return Ok(stored);
                }
            }
            Ok(None)
        })
    }

    fn last_message_id(&self, session: &str) -> Result<Option<String>> {
        stored(|| {
            Ok(self
                .0
                .prepare_cached(
                    "select id from message where session_id = ? order by sequence desc limit 1",
                )?
                .query_row([session], |r| r.get(0))
                .optional()?)
        })
    }

    fn tool_part(
        &self,
        session: &str,
        message: Option<&str>,
        call: &str,
    ) -> Result<Option<PartRow>> {
        stored(|| {
            let row = |r: &rusqlite::Row<'_>| Ok((r.get(0)?, r.get(1)?, r.get(2)?));
            Ok(match message {
                Some(message) => self
                    .0
                    .prepare_cached(
                        "select id, message_id, data from part where message_id = ? and session_id = ?
              and (case when json_valid(data) then json_extract(data, '$.type') end) = 'tool'
              and (case when json_valid(data) then json_extract(data, '$.callID') end) = ?
              order by sequence desc, rowid desc limit 1",
                    )?
                    .query_row([message, session, call], row)
                    .optional()?,
                None => self
                    .0
                    .prepare_cached(
                        "select id, message_id, data from part where session_id = ?
              and (case when json_valid(data) then json_extract(data, '$.type') end) = 'tool'
              and (case when json_valid(data) then json_extract(data, '$.callID') end) = ?
              order by sequence desc, rowid desc limit 1",
                    )?
                    .query_row([session, call], row)
                    .optional()?,
            })
        })
    }

    fn settled_tool_parts(&self, session: &str, message: &str) -> Result<u64> {
        stored(|| {
            let count: i64 = self
                .0
                .prepare_cached(
                    "select count(*) from part where message_id = ? and session_id = ?
              and (case when json_valid(data) then json_extract(data, '$.type') end) = 'tool'
              and (case when json_valid(data) then json_extract(data, '$.state.status') end)
                in ('completed', 'error')",
                )?
                .query_row([message, session], |r| r.get(0))?;
            Ok(u64::try_from(count).unwrap_or(0))
        })
    }

    fn result_owner(&self, session: &str, id: &str) -> Result<Option<String>> {
        stored(|| {
            Ok(self
                .0
                .query_row(
                    "select message_id from part where session_id = ?1
                  and (json_extract(data, '$.metadata.rustCli.resultMessageId') = ?2
                    or (id = ?2 and json_extract(data, '$.type') = 'tool'))
                  limit 1",
                    params![session, id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?)
        })
    }

    fn save_entry(&self, entry: &Entry) -> Result<()> {
        stored(|| repo::entries::save(self.0, entry))
    }

    fn entries(&self, session: &str, kind: Option<&str>) -> Result<Vec<Entry>> {
        stored(|| repo::entries::list(self.0, session, kind))
    }

    fn entry_data(&self, session: &str, id: &str) -> Result<Option<String>> {
        stored(|| {
            Ok(session_entry::Entity::find_by_id(id)
                .filter(session_entry::Column::SessionId.eq(session))
                .one(&Db(self.0))?
                .map(|row| row.data))
        })
    }

    fn max_sequence_number(&self, session: &str, kinds: [&str; 2]) -> Result<Option<i64>> {
        stored(|| {
            Ok(self
                .0
                .prepare_cached(
                    "select max(cast(json_extract(data, '$.sequenceNumber') as integer)) from session_entry
                 where session_id = ? and type in (?, ?)",
                )?
                .query_row(params![session, kinds[0], kinds[1]], |row| row.get(0))?)
        })
    }

    fn read_setting(
        &self,
        scope: &str,
        id: &str,
        namespace: &str,
        key: &str,
    ) -> Result<Option<String>> {
        stored(|| repo::settings::read(self.0, scope, id, namespace, key))
    }

    fn write_setting(
        &self,
        (scope, id, namespace, key): (&str, &str, &str, &str),
        value: String,
        now: i64,
    ) -> Result<()> {
        stored(|| repo::settings::write(self.0, scope, id, namespace, key, value, now))
    }

    fn settings_in(
        &self,
        scope: &str,
        namespace: &str,
        key: &str,
        ids: &[&str],
    ) -> Result<Vec<(String, String)>> {
        stored(|| {
            Ok(local_setting::Entity::find()
                .filter(local_setting::Column::Scope.eq(scope))
                .filter(local_setting::Column::Namespace.eq(namespace))
                .filter(local_setting::Column::Key.eq(key))
                .filter(local_setting::Column::ScopeId.is_in(ids.iter().copied()))
                .all(&Db(self.0))?
                .into_iter()
                .map(|row| (row.scope_id, row.value))
                .collect())
        })
    }

    fn legacy_permission(&self, project: &str) -> Result<Option<String>> {
        stored(|| {
            Ok(permission::Entity::find_by_id(project)
                .one(&Db(self.0))?
                .map(|row| row.data))
        })
    }

    fn session_target(&self, session: &str) -> Result<Option<TargetRow>> {
        stored(|| {
            Ok(session_target::Entity::find_by_id(session)
                .one(&Db(self.0))?
                .map(|row| TargetRow {
                    session_id: row.session_id,
                    target_id: row.target_id,
                    objective: row.objective,
                    summary_title: row.summary_title,
                    status: row.status,
                    token_budget: row.token_budget,
                    tokens_used: row.tokens_used,
                    time_used_seconds: row.time_used_seconds,
                    time_created: row.time_created,
                    time_updated: row.time_updated,
                }))
        })
    }

    fn insert_session_target(&self, row: &TargetRow) -> Result<()> {
        stored(|| {
            session_target::Entity::insert(session_target::ActiveModel {
                session_id: Set(row.session_id.clone()),
                target_id: Set(row.target_id.clone()),
                objective: Set(row.objective.clone()),
                summary_title: Set(row.summary_title.clone()),
                status: Set(row.status.clone()),
                token_budget: Set(row.token_budget),
                tokens_used: Set(row.tokens_used),
                time_used_seconds: Set(row.time_used_seconds),
                time_created: Set(row.time_created),
                time_updated: Set(row.time_updated),
                ..Default::default()
            })
            .exec(&Db(self.0))?;
            Ok(())
        })
    }

    fn record_model(&self, row: &ModelRow) -> Result<()> {
        stored(|| repo::usage::record_model(self.0, row))
    }

    fn upsert_tool(&self, row: &ToolRow) -> Result<()> {
        stored(|| repo::usage::upsert_tool(self.0, row))
    }

    fn upsert_turn(&self, row: &TurnRow) -> Result<()> {
        stored(|| repo::usage::upsert_turn(self.0, row))
    }

    fn prune_usage(&self, before: i64) -> Result<()> {
        stored(|| repo::usage::prune(self.0, before))
    }

    fn turn_totals(&self, session: &str, turn: &str) -> Result<TurnTotals> {
        stored(|| repo::usage::turn_totals(self.0, session, turn))
    }

    fn turn_started(&self, session: &str, turn: &str) -> Result<Option<i64>> {
        stored(|| repo::usage::turn_started(self.0, session, turn))
    }

    fn turn_failed(&self, session: &str, user_message: &str) -> Result<bool> {
        stored(|| repo::usage::turn_failed(self.0, session, user_message))
    }

    fn savepoint(&self) -> Result<()> {
        stored(|| Ok(self.0.execute_batch("savepoint rust_cli_usage")?))
    }

    fn release_savepoint(&self) -> Result<()> {
        stored(|| Ok(self.0.execute_batch("release rust_cli_usage")?))
    }

    // SQLite 已经自己回滚了整个事务（磁盘满、IO 错误等）时 savepoint 不在了，`rollback to` 失败（usage.md §2.5）。
    fn rollback_savepoint(&self) -> Result<()> {
        stored(|| {
            Ok(self
                .0
                .execute_batch("rollback to rust_cli_usage; release rust_cli_usage")?)
        })
    }
}
