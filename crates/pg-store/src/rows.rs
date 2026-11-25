//! [`Rows`] 的 PostgreSQL 实现（pg-store.md §12）：与本机 node-store 相同的 Node 行，另加 tenant_id。编排（node-records）
//! 是同步的，跑在阻塞线程里；每个行操作在同一个事务上经运行时句柄执行。ORM 管普通的增删改查，原生 SQL 只用于 JSON 路径、
//! Node 的合并语义（保留 legacy 成员、模型选择只换一个成员）与聚合，对应 node-store 里同样保留的原生 SQL。
use std::collections::HashMap;
use std::future::Future;

use rust_cli_node_records::codecs::{TASK_TYPES, decode_entry_data, decode_message};
use rust_cli_node_records::raw::{MessageHead, RawPart, RawRecord};
use rust_cli_node_records::rows::{
    Create, Entry, ForkSource, ModelRow, PartRow, Result, Rows, SessionRow, TargetRow, ToolRow,
    TurnRow, TurnTotals, Update,
};
use sea_orm::sea_query::{Expr, ExprTrait, Func, OnConflict};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect,
    Set,
};
use serde_json::Value;
use sqlx::Row as _;

use crate::PgSessionStore;
use crate::entities::{
    local_setting, message, part, permission, session, session_entry, session_target,
};
use crate::transaction::Tx;

/// 一个事务上的行操作；只在阻塞线程里使用（`block_on` 不能在异步线程上调用）。
pub(crate) struct PgRows<'a> {
    pub(crate) store: &'a PgSessionStore,
    pub(crate) tx: &'a Tx,
    pub(crate) runtime: tokio::runtime::Handle,
}

fn decode_session(row: session::Model) -> Result<SessionRow> {
    let nonempty = |value: Option<String>| value.filter(|text| !text.is_empty());
    Ok(SessionRow {
        id: row.id,
        workspace_id: nonempty(row.workspace_id),
        parent_id: nonempty(row.parent_id),
        trace_id: nonempty(row.trace_id),
        task_type: if TASK_TYPES.contains(&row.task_type.as_str()) {
            row.task_type
        } else {
            "interactive".into()
        },
        directory: row.directory,
        title: row.title,
        title_source: Some(row.title_source),
        revert: row
            .revert
            .map(|text| serde_json::from_str(&text))
            .transpose()
            .map_err(invalid)?,
        permission: row
            .permission
            .map(|text| serde_json::from_str(&text))
            .transpose()
            .map_err(invalid)?,
        last_outcome: row.last_outcome,
        time_created: row.time_created,
        time_updated: row.time_updated,
        time_deleted: row.time_deleted,
    })
}

/// 读出的行解不开、要写的记录不合形状：存储错误，文字是原错误的文字（与本机库相同）。
pub(crate) fn invalid(error: impl std::fmt::Display) -> rust_cli_kernel_api::store::StoreError {
    crate::storage(error.to_string())
}

/// JSON 文本里的一个成员（node-store 的 `case when json_valid(data) then json_extract(...) end`）。
fn json_path(column: &str, path: &str) -> String {
    let keys: Vec<String> = path.split('.').map(|key| format!("'{key}'")).collect();
    format!(
        "(case when {column} is json then {column}::jsonb #>> array[{}] end)",
        keys.join(",")
    )
}

impl PgRows<'_> {
    /// 数据库错误按 [`crate::orm::error`] 分类（断线是 Unavailable，§4）：行操作的结果直接是存储错误，编排用 `?` 原样
    /// 交出。以前先包进 `anyhow`，编排再用 `Any` 向下转型找回类别。
    pub(crate) fn run<T>(
        &self,
        work: impl Future<Output = std::result::Result<T, DbErr>>,
    ) -> Result<T> {
        self.runtime.block_on(work).map_err(crate::orm::error)
    }

    pub(crate) fn tenant(&self) -> &str {
        self.store.tenant()
    }

    pub(crate) fn touch(&self, id: &str, time_updated: i64) -> Result<()> {
        self.run(
            self.store
                .update::<session::Entity>()
                .filter(session::Column::Id.eq(id))
                .col_expr(
                    session::Column::TimeUpdated,
                    Expr::expr(Func::greatest([
                        Expr::col(session::Column::TimeUpdated),
                        Expr::val(time_updated),
                    ])),
                )
                .exec(self.tx),
        )?;
        Ok(())
    }

    pub(crate) fn next_sequence(&self, sql: &str, owner: &str) -> Result<i64> {
        let row = self.run(
            crate::orm::native(sql)
                .bind(self.tenant())
                .bind(owner)
                .fetch_one(self.tx),
        )?;
        Ok(row.get::<i64, _>("next"))
    }

    /// 这个会话的 part，按 Node `messages` 的行序：按消息分组，组内 `sequence`（NULL 在后）、创建时间、id。
    fn ordered_parts(&self, session: &str) -> sea_orm::Select<part::Entity> {
        self.store
            .find::<part::Entity>()
            .filter(part::Column::SessionId.eq(session))
            .order_by_asc(part::Column::MessageId)
            .order_by(Expr::col(part::Column::Sequence).is_null(), Order::Asc)
            .order_by_asc(part::Column::Sequence)
            .order_by_asc(part::Column::TimeCreated)
            .order_by_asc(part::Column::Id)
    }

    /// 按两个查询读消息行与 part 行，part 按消息分组、只留 data 原文（[`Rows::messages_raw`] 与 [`Rows::messages_raw_of`] 共用）。
    fn raw(
        &self,
        session: &str,
        parts_query: sea_orm::Select<part::Entity>,
        messages_query: sea_orm::Select<message::Entity>,
    ) -> Result<Vec<RawRecord>> {
        let mut parts: HashMap<String, Vec<RawPart>> = HashMap::new();
        for row in self.run(parts_query.all(self.tx))? {
            parts.entry(row.message_id).or_default().push(RawPart {
                id: row.id,
                data: row.data,
            });
        }
        self.run(messages_query.all(self.tx))?
            .into_iter()
            .map(|row| {
                Ok(RawRecord {
                    info: decode_message(&row.data, &row.id, &row.session_id).map_err(invalid)?,
                    session: session.to_owned(),
                    parts: parts.remove(&row.id).unwrap_or_default(),
                })
            })
            .collect()
    }

    /// 消息的行序（Node `messages`）：`sequence`（NULL 在后）、创建时间、行序（替代 rowid）。
    fn ordered_messages(&self, session: &str) -> sea_orm::Select<message::Entity> {
        self.store
            .find::<message::Entity>()
            .filter(message::Column::SessionId.eq(session))
            .order_by(Expr::col(message::Column::Sequence).is_null(), Order::Asc)
            .order_by_asc(message::Column::Sequence)
            .order_by_asc(message::Column::TimeCreated)
            .order_by_asc(message::Column::RowSeq)
    }

    fn sessions(&self, query: sea_orm::Select<session::Entity>) -> Result<Vec<SessionRow>> {
        // 只列绑定项目的会话（租户之内的项目隔离，pg-store.md §2）。
        let query = query.filter(session::Column::ProjectId.is_in(self.store.project_ids()));
        self.run(query.all(self.tx))?
            .into_iter()
            .map(decode_session)
            .collect()
    }
}

impl Rows for PgRows<'_> {
    fn session(&self, id: &str) -> Result<Option<SessionRow>> {
        self.run(
            self.store
                .find::<session::Entity>()
                .filter(session::Column::Id.eq(id))
                .one(self.tx),
        )?
        .map(decode_session)
        .transpose()
    }

    fn fork_source(&self, id: &str) -> Result<Option<ForkSource>> {
        Ok(self
            .run(
                self.store
                    .find::<session::Entity>()
                    .filter(session::Column::Id.eq(id))
                    .filter(session::Column::TimeDeleted.is_null())
                    .one(self.tx),
            )?
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
    }

    fn session_project_id(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .run(
                self.store
                    .find::<session::Entity>()
                    .filter(session::Column::Id.eq(id))
                    .one(self.tx),
            )?
            .map(|row| row.project_id))
    }
    fn create_session(&self, input: &Create, now: i64) -> Result<()> {
        self.write_session(input, now)
    }
    fn update_session(&self, input: &Update, now: i64) -> Result<usize> {
        self.change_session(input, now)
    }

    fn touch_session(&self, id: &str, time_updated: i64) -> Result<()> {
        self.touch(id, time_updated)
    }

    fn soft_delete_session(&self, id: &str, time_deleted: i64) -> Result<()> {
        self.run(
            self.store
                .update::<session::Entity>()
                .filter(session::Column::Id.eq(id))
                .col_expr(
                    session::Column::TimeDeleted,
                    Expr::expr(Func::coalesce([
                        Expr::col(session::Column::TimeDeleted),
                        Expr::val(time_deleted),
                    ])),
                )
                .exec(self.tx),
        )?;
        Ok(())
    }

    fn list_sessions(&self, directory: &str, limit: Option<i64>) -> Result<Vec<SessionRow>> {
        let mut query = self
            .store
            .find::<session::Entity>()
            .filter(session::Column::TimeDeleted.is_null())
            .filter(
                sea_orm::Condition::any()
                    .add(session::Column::ParentId.is_null())
                    .add(session::Column::TaskType.eq("fork")),
            )
            .order_by_desc(session::Column::TimeUpdated)
            .order_by_desc(session::Column::Id);
        if !directory.is_empty() {
            query = query.filter(session::Column::Directory.eq(directory));
        }
        if let Some(limit) = limit.filter(|limit| *limit > 0) {
            query = query.limit(limit as u64);
        }
        self.sessions(query)
    }

    fn query_sessions(
        &self,
        directory: Option<&str>,
        created: bool,
        limit: Option<i64>,
    ) -> Result<Vec<SessionRow>> {
        let mut query = self
            .store
            .find::<session::Entity>()
            .filter(session::Column::TimeDeleted.is_null())
            .filter(session::Column::TaskType.is_in(["interactive", "fork", "workflow_parent"]))
            .order_by_desc(if created {
                session::Column::TimeCreated
            } else {
                session::Column::TimeUpdated
            })
            .order_by_desc(session::Column::Id);
        if let Some(directory) = directory {
            query = query.filter(session::Column::Directory.eq(directory));
        }
        if let Some(limit) = limit.filter(|limit| *limit >= 0) {
            query = query.limit(limit as u64);
        }
        self.sessions(query)
    }

    fn first_prompt_part(&self, session: &str) -> Result<Option<String>> {
        let sql = format!(
            "select p.data from zcode.message m join zcode.part p
               on p.tenant_id = m.tenant_id and p.message_id = m.id
             where m.tenant_id = $1 and m.session_id = $2
               and {} = 'user' and {} = 'real_user' and {} = 'text'
               and coalesce({}, 'false') = 'false'
             order by m.time_created, m.id, p.sequence is null, p.sequence, p.time_created, p.id
             limit 1",
            json_path("m.data", "role"),
            json_path("m.data", "semantics.origin"),
            json_path("p.data", "type"),
            json_path("p.data", "synthetic"),
        );
        let row = self.run(
            crate::orm::native(&sql)
                .bind(self.tenant())
                .bind(session)
                .fetch_optional(self.tx),
        )?;
        Ok(row.map(|row| row.get("data")))
    }
    fn sessions_by_ids(&self, ids: &[&str]) -> Result<Vec<SessionRow>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.sessions(
            self.store
                .find::<session::Entity>()
                .filter(session::Column::Id.is_in(ids.iter().copied())),
        )
    }

    /// 首条真实用户输入的创建时间（同 node-store：`order by time_created, id`）。
    fn first_input_at(&self, session: &str) -> Result<Option<i64>> {
        let sql = format!(
            "select time_created from zcode.message
             where tenant_id = $1 and session_id = $2 and {} = 'user' and {} = 'real_user'
             order by time_created, id limit 1",
            json_path("data", "role"),
            json_path("data", "semantics.origin"),
        );
        let row = self.run(
            crate::orm::native(&sql)
                .bind(self.tenant())
                .bind(session)
                .fetch_optional(self.tx),
        )?;
        Ok(row.map(|row| row.get("time_created")))
    }

    fn first_message(&self, session: &str) -> Result<Option<(String, String)>> {
        Ok(self
            .run(
                self.store
                    .find::<message::Entity>()
                    .filter(message::Column::SessionId.eq(session))
                    .order_by_asc(message::Column::TimeCreated)
                    .order_by_asc(message::Column::Id)
                    .one(self.tx),
            )?
            .map(|row| (row.id, row.data)))
    }

    fn part_metadata_is(&self, message: &str, key: &str, value: &str) -> Result<bool> {
        let sql = format!(
            "select 1 as found from zcode.part where tenant_id = $1 and message_id = $2 and {} = $3 limit 1",
            json_path("data", &format!("metadata.{key}")),
        );
        let row = self.run(
            crate::orm::native(&sql)
                .bind(self.tenant())
                .bind(message)
                .bind(value)
                .fetch_optional(self.tx),
        )?;
        Ok(row.is_some())
    }

    fn save_message(&self, info: &Value, now: i64) -> Result<()> {
        self.write_message(info, now)
    }
    fn save_part(&self, value: &Value, now: i64) -> Result<()> {
        self.write_part(value, now)
    }

    /// Node `messages`：按 `sequence` 排序（NULL 在后），同序时按创建时间与行序（替代 rowid）；part 只留 data 原文，用到时再解码
    /// （node-store.md §5.4）。
    fn messages_raw(&self, session: &str) -> Result<Vec<RawRecord>> {
        self.raw(
            session,
            self.ordered_parts(session),
            self.ordered_messages(session),
        )
    }

    /// 给出的这些消息与它们的 part（按 id，行序同 [`Rows::messages_raw`]；node-store.md §5.5）。
    fn messages_raw_of(&self, session: &str, ids: &[&str]) -> Result<Vec<RawRecord>> {
        self.raw(
            session,
            self.ordered_parts(session)
                .filter(part::Column::MessageId.is_in(ids.iter().copied())),
            self.ordered_messages(session)
                .filter(message::Column::Id.is_in(ids.iter().copied())),
        )
    }

    /// 消息 id，按 [`Rows::messages_raw`] 的消息行序（子代理的冷读只按 id 选分支）。
    fn message_ids(&self, session: &str) -> Result<Vec<String>> {
        self.run(
            self.ordered_messages(session)
                .select_only()
                .column(message::Column::Id)
                .into_tuple()
                .all(self.tx),
        )
    }

    /// 消息行的头（id、role、summary），按 [`Rows::messages_raw`] 的消息行序；不读 part、不建整份 info（范围读的历史索引，node-store.md §5.6）。
    fn message_heads(&self, session: &str) -> Result<Vec<MessageHead>> {
        self.run(self.ordered_messages(session).all(self.tx))?
            .into_iter()
            .map(|row| MessageHead::parse(row.id, &row.data).map_err(invalid))
            .collect()
    }

    /// `type = tool`、`tool` 在 `tools` 里的 part（原文）与它所在消息的 id，按 [`Rows::messages_raw`] 的 part 行序；data 不是合法 JSON 的不选
    /// （`json_path`，同 node-store）。
    fn tool_parts(&self, session: &str, tools: &[&str]) -> Result<Vec<(String, RawPart)>> {
        let rows = self.run(
            self.ordered_parts(session)
                .filter(Expr::cust(json_path("data", "type")).eq("tool"))
                .filter(Expr::cust(json_path("data", "tool")).is_in(tools.iter().copied()))
                .all(self.tx),
        )?;
        Ok(rows
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

    fn message_exists(&self, id: &str) -> Result<bool> {
        Ok(self
            .run(
                self.store
                    .find::<message::Entity>()
                    .filter(message::Column::Id.eq(id))
                    .one(self.tx),
            )?
            .is_some())
    }

    fn message_data(&self, session: &str, id: &str) -> Result<Option<String>> {
        Ok(self
            .run(
                self.store
                    .find::<message::Entity>()
                    .filter(message::Column::Id.eq(id))
                    .filter(message::Column::SessionId.eq(session))
                    .one(self.tx),
            )?
            .map(|row| row.data))
    }

    fn current_user_row(&self, session: &str) -> Result<Option<(String, String)>> {
        let sql = format!(
            "select id, data from zcode.message where tenant_id = $1 and session_id = $2
               and {} = 'user' and {} = 'real_user'
             order by sequence is null desc, sequence desc, time_created desc, row_seq desc limit 1",
            json_path("data", "role"),
            json_path("data", "semantics.origin"),
        );
        let row = self.run(
            crate::orm::native(&sql)
                .bind(self.tenant())
                .bind(session)
                .fetch_optional(self.tx),
        )?;
        Ok(row.map(|row| (row.get("id"), row.get("data"))))
    }

    fn last_message_id(&self, session: &str) -> Result<Option<String>> {
        Ok(self
            .run(
                self.store
                    .find::<message::Entity>()
                    .filter(message::Column::SessionId.eq(session))
                    // SQLite 的 `order by sequence desc` 把 NULL 排在最后；PG 的降序默认在最前。
                    .order_by(Expr::col(message::Column::Sequence).is_null(), Order::Asc)
                    .order_by_desc(message::Column::Sequence)
                    .one(self.tx),
            )?
            .map(|row| row.id))
    }

    fn tool_part(
        &self,
        session: &str,
        message: Option<&str>,
        call: &str,
    ) -> Result<Option<PartRow>> {
        let sql = format!(
            "select id, message_id, data from zcode.part where tenant_id = $1 and session_id = $2
               and ($3::text is null or message_id = $3) and {} = 'tool' and {} = $4
             order by sequence desc nulls last, row_seq desc limit 1",
            json_path("data", "type"),
            json_path("data", "callID"),
        );
        let row = self.run(
            crate::orm::native(&sql)
                .bind(self.tenant())
                .bind(session)
                .bind(message.map(str::to_owned))
                .bind(call)
                .fetch_optional(self.tx),
        )?;
        Ok(row.map(|row| (row.get("id"), row.get("message_id"), row.get("data"))))
    }

    fn settled_tool_parts(&self, session: &str, message: &str) -> Result<u64> {
        let sql = format!(
            "select count(*)::bigint as settled from zcode.part
             where tenant_id = $1 and message_id = $2 and session_id = $3
               and {} = 'tool' and {} in ('completed', 'error')",
            json_path("data", "type"),
            json_path("data", "state.status"),
        );
        let row = self.run(
            crate::orm::native(&sql)
                .bind(self.tenant())
                .bind(message)
                .bind(session)
                .fetch_one(self.tx),
        )?;
        Ok(u64::try_from(row.get::<i64, _>("settled")).unwrap_or(0))
    }

    fn result_owner(&self, session: &str, id: &str) -> Result<Option<String>> {
        let sql = format!(
            "select message_id from zcode.part where tenant_id = $1 and session_id = $2
               and ({} = $3 or (id = $3 and {} = 'tool')) limit 1",
            json_path("data", "metadata.rustCli.resultMessageId"),
            json_path("data", "type"),
        );
        let row = self.run(
            crate::orm::native(&sql)
                .bind(self.tenant())
                .bind(session)
                .bind(id)
                .fetch_optional(self.tx),
        )?;
        Ok(row.map(|row| row.get("message_id")))
    }
    fn save_entry(&self, entry: &Entry) -> Result<()> {
        self.write_entry(entry)
    }

    /// Node `sessionEntries`（`order by time_created, rowid`；行序替代 rowid）。
    fn entries(&self, session: &str, kind: Option<&str>) -> Result<Vec<Entry>> {
        let mut query = self
            .store
            .find::<session_entry::Entity>()
            .filter(session_entry::Column::SessionId.eq(session))
            .order_by_asc(session_entry::Column::TimeCreated)
            .order_by_asc(session_entry::Column::RowSeq);
        if let Some(kind) = kind {
            query = query.filter(session_entry::Column::Kind.eq(kind));
        }
        self.run(query.all(self.tx))?
            .into_iter()
            .map(|row| {
                Ok(Entry {
                    data: decode_entry_data(&row.kind, &row.data).map_err(invalid)?,
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

    fn entry_data(&self, session: &str, id: &str) -> Result<Option<String>> {
        Ok(self
            .run(
                self.store
                    .find::<session_entry::Entity>()
                    .filter(session_entry::Column::Id.eq(id))
                    .filter(session_entry::Column::SessionId.eq(session))
                    .one(self.tx),
            )?
            .map(|row| row.data))
    }

    fn max_sequence_number(&self, session: &str, kinds: [&str; 2]) -> Result<Option<i64>> {
        let sql = format!(
            "select max(({})::bigint) as max from zcode.session_entry
             where tenant_id = $1 and session_id = $2 and type in ($3, $4)",
            json_path("data", "sequenceNumber"),
        );
        let row = self.run(
            crate::orm::native(&sql)
                .bind(self.tenant())
                .bind(session)
                .bind(kinds[0])
                .bind(kinds[1])
                .fetch_one(self.tx),
        )?;
        Ok(row.get("max"))
    }

    fn read_setting(
        &self,
        scope: &str,
        id: &str,
        namespace: &str,
        key: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .run(
                self.store
                    .find::<local_setting::Entity>()
                    .filter(local_setting::Column::Scope.eq(scope))
                    .filter(local_setting::Column::ScopeId.eq(id))
                    .filter(local_setting::Column::Namespace.eq(namespace))
                    .filter(local_setting::Column::Key.eq(key))
                    .one(self.tx),
            )?
            .map(|row| row.value))
    }

    fn write_setting(
        &self,
        (scope, id, namespace, key): (&str, &str, &str, &str),
        value: String,
        now: i64,
    ) -> Result<()> {
        self.run(
            local_setting::Entity::insert(local_setting::ActiveModel {
                tenant_id: Set(self.tenant().to_owned()),
                scope: Set(scope.into()),
                scope_id: Set(id.into()),
                namespace: Set(namespace.into()),
                key: Set(key.into()),
                value: Set(value),
                schema_version: Set(1),
                time_created: Set(now),
                time_updated: Set(now),
            })
            .on_conflict(
                OnConflict::columns([
                    local_setting::Column::TenantId,
                    local_setting::Column::Scope,
                    local_setting::Column::ScopeId,
                    local_setting::Column::Namespace,
                    local_setting::Column::Key,
                ])
                .update_columns([
                    local_setting::Column::Value,
                    local_setting::Column::SchemaVersion,
                    local_setting::Column::TimeUpdated,
                ])
                .to_owned(),
            )
            .exec(self.tx),
        )?;
        Ok(())
    }

    fn settings_in(
        &self,
        scope: &str,
        namespace: &str,
        key: &str,
        ids: &[&str],
    ) -> Result<Vec<(String, String)>> {
        Ok(self
            .run(
                self.store
                    .find::<local_setting::Entity>()
                    .filter(local_setting::Column::Scope.eq(scope))
                    .filter(local_setting::Column::Namespace.eq(namespace))
                    .filter(local_setting::Column::Key.eq(key))
                    .filter(local_setting::Column::ScopeId.is_in(ids.iter().copied()))
                    .all(self.tx),
            )?
            .into_iter()
            .map(|row| (row.scope_id, row.value))
            .collect())
    }

    fn legacy_permission(&self, project: &str) -> Result<Option<String>> {
        Ok(self
            .run(
                self.store
                    .find::<permission::Entity>()
                    .filter(permission::Column::ProjectId.eq(project))
                    .one(self.tx),
            )?
            .map(|row| row.data))
    }

    fn session_target(&self, session: &str) -> Result<Option<TargetRow>> {
        Ok(self
            .run(
                self.store
                    .find::<session_target::Entity>()
                    .filter(session_target::Column::SessionId.eq(session))
                    .one(self.tx),
            )?
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
    }

    fn insert_session_target(&self, row: &TargetRow) -> Result<()> {
        self.run(
            session_target::Entity::insert(session_target::ActiveModel {
                tenant_id: Set(self.tenant().to_owned()),
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
            .exec(self.tx),
        )?;
        Ok(())
    }

    fn record_model(&self, row: &ModelRow) -> Result<()> {
        self.run(crate::usage_rows::record_model(self.store, self.tx, row))
    }

    fn upsert_tool(&self, row: &ToolRow) -> Result<()> {
        self.run(crate::usage_rows::upsert_tool(self.store, self.tx, row))
    }

    fn upsert_turn(&self, row: &TurnRow) -> Result<()> {
        self.run(crate::usage_rows::upsert_turn(self.store, self.tx, row))
    }

    fn prune_usage(&self, before: i64) -> Result<()> {
        self.run(crate::usage_rows::prune(self.store, self.tx, before))
    }

    fn turn_totals(&self, session: &str, turn: &str) -> Result<TurnTotals> {
        self.run(crate::usage_rows::turn_totals(
            self.store, self.tx, session, turn,
        ))
    }

    fn turn_started(&self, session: &str, turn: &str) -> Result<Option<i64>> {
        self.run(crate::usage_rows::turn_started(
            self.store, self.tx, session, turn,
        ))
    }

    fn turn_failed(&self, session: &str, user_message: &str) -> Result<bool> {
        self.run(crate::usage_rows::turn_failed(
            self.store,
            self.tx,
            session,
            user_message,
        ))
    }

    fn savepoint(&self) -> Result<()> {
        self.run(self.tx.execute_unprepared("savepoint rust_cli_usage"))?;
        Ok(())
    }

    fn release_savepoint(&self) -> Result<()> {
        self.run(
            self.tx
                .execute_unprepared("release savepoint rust_cli_usage"),
        )?;
        Ok(())
    }

    fn rollback_savepoint(&self) -> Result<()> {
        self.run(self.tx.execute_unprepared(
            "rollback to savepoint rust_cli_usage; release savepoint rust_cli_usage",
        ))?;
        Ok(())
    }
}
