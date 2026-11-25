//! [`PgRows`] 的写入：会话行与 Node 的 message / part / session_entry upsert（同 node-store `repo/` 的语义）。从 `rows.rs`
//! 拆出（单个文件的行数上限）。
use rust_cli_base::js::json::{encode, stringify};
use rust_cli_node_records::codecs::MODEL_SELECTION_ENTRY;
use rust_cli_node_records::rows::{Create, Entry, Result, Update};
use rust_cli_node_records::stored::{self, MESSAGE_LEGACY, PART_LEGACY, preserve_legacy};
use sea_orm::sea_query::{Expr, ExprTrait, Func, OnConflict};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::{Value, json};

use crate::entities::{message, part, session, session_entry};
use crate::rows::{PgRows, invalid};

impl PgRows<'_> {
    /// Node `createSession`（upsert，保留首次的 trace 与 time_created；同 node-store）。
    pub(crate) fn write_session(&self, input: &Create, now: i64) -> Result<()> {
        let created = input.time_created.unwrap_or(now);
        let titled = input
            .title_source
            .as_deref()
            .is_some_and(|source| !source.is_empty());
        let excluded = |column: session::Column| {
            Expr::col((sea_orm::sea_query::Alias::new("excluded"), column))
        };
        let current = |column: session::Column| Expr::col((session::Entity, column));
        self.run(
            session::Entity::insert(session::ActiveModel {
                tenant_id: Set(self.tenant().to_owned()),
                id: Set(input.id.clone()),
                project_id: Set(input.project_id.clone()),
                workspace_id: Set(input.workspace_id.clone()),
                parent_id: Set(input.parent_id.clone()),
                trace_id: Set(input.trace_id.clone()),
                task_type: Set(input
                    .task_type
                    .clone()
                    .unwrap_or_else(|| "interactive".into())),
                slug: Set(input.slug.clone()),
                directory: Set(input.directory.clone()),
                path: Set(input.path.clone()),
                title: Set(input.title.clone()),
                title_source: Set(input
                    .title_source
                    .clone()
                    .unwrap_or_else(|| "first_input".into())),
                title_message_id: Set(None),
                version: Set(input.version.clone()),
                share_url: Set(None),
                permission: Set(input.permission.as_ref().and_then(encode)),
                time_created: Set(created),
                time_updated: Set(created),
                time_title_updated: Set(titled.then_some(created)),
                ..Default::default()
            })
            .on_conflict(
                OnConflict::columns([session::Column::TenantId, session::Column::Id])
                    .update_columns([
                        session::Column::ProjectId,
                        session::Column::WorkspaceId,
                        session::Column::ParentId,
                        session::Column::TaskType,
                        session::Column::Slug,
                        session::Column::Directory,
                        session::Column::Path,
                        session::Column::Title,
                        session::Column::TitleSource,
                        session::Column::TitleMessageId,
                        session::Column::Version,
                        session::Column::ShareUrl,
                        session::Column::TimeTitleUpdated,
                        session::Column::TimeUpdated,
                    ])
                    .value(
                        session::Column::TraceId,
                        Expr::expr(Func::coalesce([
                            current(session::Column::TraceId),
                            excluded(session::Column::TraceId),
                        ])),
                    )
                    .value(
                        session::Column::Permission,
                        Expr::expr(Func::coalesce([
                            excluded(session::Column::Permission),
                            current(session::Column::Permission),
                        ])),
                    )
                    .to_owned(),
            )
            .exec(self.tx),
        )?;
        Ok(())
    }

    /// Node `updateSession`（同 node-store：只改给出的列，标题按来源 CAS）。
    pub(crate) fn change_session(&self, input: &Update, now: i64) -> Result<usize> {
        let mut query = self
            .store
            .update::<session::Entity>()
            .filter(session::Column::Id.eq(&input.id));
        let mut changed = false;
        for (column, value) in [
            (session::Column::Title, &input.title),
            (session::Column::TitleSource, &input.title_source),
            (session::Column::TitleMessageId, &input.title_message_id),
            (session::Column::LastOutcome, &input.last_outcome),
        ] {
            if let Some(value) = value {
                query = query.col_expr(column, Expr::val(value.clone()));
                changed = true;
            }
        }
        for (column, value) in [
            (session::Column::Revert, &input.revert),
            (session::Column::Permission, &input.permission),
        ] {
            if let Some(value) = value {
                query = query.col_expr(column, Expr::val(encode(value)));
                changed = true;
            }
        }
        if input.title.is_some() || input.title_source.is_some() {
            query = query.col_expr(session::Column::TimeTitleUpdated, Expr::val(now));
        }
        if let Some(at) = input.time_updated {
            query = query.col_expr(
                session::Column::TimeUpdated,
                Expr::expr(Func::greatest([
                    Expr::col(session::Column::TimeUpdated),
                    Expr::val(at),
                ])),
            );
            changed = true;
        }
        if !changed {
            return Ok(0);
        }
        if input.title.is_some() && !input.expected_title_sources.is_empty() {
            query = query.filter(
                Expr::expr(Func::coalesce([
                    Expr::col(session::Column::TitleSource),
                    Expr::val("first_input"),
                ]))
                .is_in(input.expected_title_sources.clone()),
            );
        }
        Ok(self.run(query.exec(self.tx))?.rows_affected as usize)
    }

    /// Node `saveMessage`（同 node-store：同一会话里覆盖时保留 legacy 成员与原 `sequence`）。
    pub(crate) fn write_message(&self, info: &Value, now: i64) -> Result<()> {
        // 各列的值与 `data` 的存储形态由 node-records 的 `stored` 算（本机库用同一份）。
        let stored::MessageRow {
            id,
            session,
            created,
            updated,
            mut data,
        } = stored::message_row(info, now)?;
        let previous = self.run(
            self.store
                .find::<message::Entity>()
                .filter(message::Column::Id.eq(id))
                .one(self.tx),
        )?;
        let sequence = match &previous {
            Some(row) if row.session_id == session => {
                data = preserve_legacy(&row.data, data, &MESSAGE_LEGACY);
                row.sequence
            }
            _ => Some(self.next_sequence(
                "select coalesce(max(sequence), -1) + 1 as next from zcode.message
                 where tenant_id = $1 and session_id = $2",
                session,
            )?),
        };
        let data = stringify(&Value::Object(data));
        self.run(
            message::Entity::insert(message::ActiveModel {
                tenant_id: Set(self.tenant().to_owned()),
                id: Set(id.to_owned()),
                session_id: Set(session.to_owned()),
                time_created: Set(created),
                time_updated: Set(updated),
                data: Set(data),
                sequence: Set(sequence),
                ..Default::default()
            })
            .on_conflict(
                OnConflict::columns([message::Column::TenantId, message::Column::Id])
                    .update_columns([
                        message::Column::SessionId,
                        message::Column::TimeUpdated,
                        message::Column::Data,
                        message::Column::Sequence,
                    ])
                    .to_owned(),
            )
            .exec(self.tx),
        )?;
        self.touch(session, updated)
    }

    /// Node `savePart`（同 node-store）。
    pub(crate) fn write_part(&self, value: &Value, now: i64) -> Result<()> {
        let stored::PartRow {
            id,
            message: owner,
            session,
            created,
            mut data,
        } = stored::part_row(value, now)?;
        let previous = self.run(
            self.store
                .find::<part::Entity>()
                .filter(part::Column::Id.eq(id))
                .one(self.tx),
        )?;
        let sequence = match &previous {
            Some(row) if row.message_id == owner && row.session_id == session => {
                data = preserve_legacy(&row.data, data, &PART_LEGACY);
                row.sequence
            }
            _ => Some(self.next_sequence(
                "select coalesce(max(sequence), -1) + 1 as next from zcode.part
                 where tenant_id = $1 and message_id = $2",
                owner,
            )?),
        };
        let data = stringify(&Value::Object(data));
        self.run(
            part::Entity::insert(part::ActiveModel {
                tenant_id: Set(self.tenant().to_owned()),
                id: Set(id.to_owned()),
                message_id: Set(owner.to_owned()),
                session_id: Set(session.to_owned()),
                time_created: Set(created),
                time_updated: Set(now),
                data: Set(data),
                sequence: Set(sequence),
                ..Default::default()
            })
            .on_conflict(
                OnConflict::columns([part::Column::TenantId, part::Column::Id])
                    .update_columns([
                        part::Column::MessageId,
                        part::Column::SessionId,
                        part::Column::TimeUpdated,
                        part::Column::Data,
                        part::Column::Sequence,
                    ])
                    .to_owned(),
            )
            .exec(self.tx),
        )?;
        self.touch(session, now)
    }

    /// Node `saveSessionEntry`（同 node-store：保留 `time_created`；模型选择只替换 `modelSelection` 成员）。
    pub(crate) fn write_entry(&self, entry: &Entry) -> Result<()> {
        let selection = entry.kind == MODEL_SELECTION_ENTRY;
        let data = if selection {
            json!({"modelSelection": entry.data})
        } else {
            entry.data.clone()
        };
        if !selection && Value::is_null(&data) {
            return Err(invalid("Session entry data must be JSON-serializable"));
        }
        let mut text = stringify(&data);
        if selection {
            let previous = self.run(
                self.store
                    .find::<session_entry::Entity>()
                    .filter(session_entry::Column::Id.eq(&entry.id))
                    .one(self.tx),
            )?;
            // 旧模型记录只替换一个 JSON 成员，保留冻结版本仍会读取的字段（node-store 的 `json_set`）。
            if let Some(row) =
                previous.filter(|row| row.kind == entry.kind && row.session_id == entry.session_id)
                && let Ok(Value::Object(mut old)) = serde_json::from_str::<Value>(&row.data)
            {
                old.insert("modelSelection".into(), data["modelSelection"].clone());
                text = stringify(&Value::Object(old));
            }
        }
        self.run(
            session_entry::Entity::insert(session_entry::ActiveModel {
                tenant_id: Set(self.tenant().to_owned()),
                id: Set(entry.id.clone()),
                session_id: Set(entry.session_id.clone()),
                kind: Set(entry.kind.clone()),
                time_created: Set(entry.time_created),
                time_updated: Set(entry.time_updated),
                data: Set(text),
                ..Default::default()
            })
            .on_conflict(
                OnConflict::columns([session_entry::Column::TenantId, session_entry::Column::Id])
                    .update_columns([
                        session_entry::Column::SessionId,
                        session_entry::Column::Kind,
                        session_entry::Column::TimeUpdated,
                        session_entry::Column::Data,
                    ])
                    .to_owned(),
            )
            .exec(self.tx),
        )?;
        if entry.touch_session {
            self.touch(&entry.session_id, entry.time_updated)?;
        }
        Ok(())
    }
}
