// 复制自 apps/zcode-cli-rust/crates/state/src/node/sessions.rs 与 session_update.rs（feat/rs 79b6691c55）。
//! `session` 行（Node `repositories/sessions.ts`，docs/specs/rust-m11-node-storage.md §4）。
//!
//! 与复制来源的差异：只保留本 crate 用到的列与操作；`createSession` 的 insert 列与 Node 当前
//! 源码一致（不再列出 `time_archived`）。
use crate::{entities::session, orm::Db};
use rusqlite::Connection;
use rust_cli_base::js::json::encode;
use sea_orm::sea_query::{Expr, ExprTrait, Func, OnConflict};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};

/// 存储形态的 Node `SessionInfo`（本 crate 读取的列）。
pub use rust_cli_node_records::rows::{Create, LISTED_TASK_TYPES, SessionRow, Update};

fn decode(row: session::Model) -> anyhow::Result<SessionRow> {
    let nonempty = |value: Option<String>| value.filter(|text| !text.is_empty());
    Ok(SessionRow {
        id: row.id,
        workspace_id: nonempty(row.workspace_id),
        parent_id: nonempty(row.parent_id),
        trace_id: nonempty(row.trace_id),
        task_type: if super::codecs::TASK_TYPES.contains(&row.task_type.as_str()) {
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
            .transpose()?,
        permission: row
            .permission
            .map(|text| serde_json::from_str(&text))
            .transpose()?,
        last_outcome: row.last_outcome,
        time_created: row.time_created,
        time_updated: row.time_updated,
        time_deleted: row.time_deleted,
    })
}
pub fn get(conn: &Connection, id: &str) -> anyhow::Result<Option<SessionRow>> {
    session::Entity::find_by_id(id)
        .one(&Db(conn))?
        .map(decode)
        .transpose()
}

/// Node `createSession`（upsert，保留首次的 trace 与 time_created）。
pub fn create(conn: &Connection, input: &Create, now: i64) -> anyhow::Result<()> {
    let created = input.time_created.unwrap_or(now);
    let titled = input
        .title_source
        .as_deref()
        .is_some_and(|source| !source.is_empty());
    session::Entity::insert(session::ActiveModel {
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
        OnConflict::column(session::Column::Id)
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
                    Expr::col((session::Entity, session::Column::TraceId)),
                    Expr::col((
                        sea_orm::sea_query::Alias::new("excluded"),
                        session::Column::TraceId,
                    )),
                ])),
            )
            .value(
                session::Column::Permission,
                Expr::expr(Func::coalesce([
                    Expr::col((
                        sea_orm::sea_query::Alias::new("excluded"),
                        session::Column::Permission,
                    )),
                    Expr::col((session::Entity, session::Column::Permission)),
                ])),
            )
            .to_owned(),
    )
    .exec(&Db(conn))?;
    Ok(())
}

/// Node `updateSession`：只更新本次输入拥有的列；标题编辑不推进活动时间。给出
/// `expected_title_sources` 时按当前标题来源做 CAS。返回改动的行数。
pub fn update(conn: &Connection, input: &Update, now: i64) -> anyhow::Result<usize> {
    let mut query = session::Entity::update_many().filter(session::Column::Id.eq(&input.id));
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
            Expr::expr(
                Func::cust(sea_orm::sea_query::Alias::new("max"))
                    .args([Expr::col(session::Column::TimeUpdated), Expr::val(at)]),
            ),
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
    Ok(query.exec(&Db(conn))?.rows_affected as usize)
}

/// 有删除凭据（永久删除中或删完）的会话不列（Node `listSessions` 的同一条件，docs/specs/rust-cli/node-store.md §5.3）。
const NOT_PURGED: &str = "not exists(select 1 from session_purge where session_id = session.id)";

/// 列表会话（本机、[`LISTED_TASK_TYPES`]；不含“没删除”）：`query` 与 `recent` 共用一份。
fn listed() -> sea_orm::Condition {
    sea_orm::Condition::all()
        .add(session::Column::WorkspaceId.is_null())
        .add(session::Column::TaskType.is_in(LISTED_TASK_TYPES))
}

/// `/resume` 与 `-c` 的会话列表：本目录的顶层会话加 fork 出的会话（`task_type = 'fork'`），排序同
/// Node `listSessions`（`order by time_updated desc, id desc`）。
///
/// fork 也作为普通会话列出；Node `listZCodeSessions`（`roots: true`）不列 fork，而 Node 的条件只能
/// 并列（`roots` 与 `taskTypes` 是“且”），所以这条查询是本工程的。子代理子会话（`subagent_child`）与
/// App 的 selection side chat 有 `parent_id` 又不是 fork，不列（docs/specs/rust-cli/session-lifecycle.md
/// §6.2）。
pub fn list(
    conn: &Connection,
    directory: &str,
    limit: Option<i64>,
) -> anyhow::Result<Vec<SessionRow>> {
    let mut query = session::Entity::find()
        .filter(session::Column::TimeDeleted.is_null())
        .filter(Expr::cust(NOT_PURGED))
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
    query.all(&Db(conn))?.into_iter().map(decode).collect()
}

/// ← 项目页与 `/resume` 页的会话（修复线 F29，docs/specs/rust-cli/node-store.md §5.1）：App 任务列表的口径
/// （Node `sessions-index` 的 `listSessions{directory, taskTypes: TASK_LIST_SESSION_TYPES, workspaceID: null}`），
/// `directory` 为 None 时不限目录（所有项目）。`created` 为真时按创建时间排。
pub fn query(
    conn: &Connection,
    directory: Option<&str>,
    created: bool,
    limit: Option<i64>,
) -> anyhow::Result<Vec<SessionRow>> {
    let mut query = session::Entity::find()
        .filter(session::Column::TimeDeleted.is_null())
        .filter(listed())
        .filter(Expr::cust(NOT_PURGED))
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
    query.all(&Db(conn))?.into_iter().map(decode).collect()
}

/// 这些 id 的会话行（含软删除的；顺序不定，调用方按自己的顺序取）。
pub fn by_ids(conn: &Connection, ids: &[&str]) -> anyhow::Result<Vec<SessionRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    session::Entity::find()
        .filter(session::Column::Id.is_in(ids.iter().copied()))
        .all(&Db(conn))?
        .into_iter()
        .map(decode)
        .collect()
}

/// 这个目录最近活动的列表会话（[`listed`]，含软删除的），排序同 [`query`]（`time_updated desc, id desc`）。
pub fn recent(conn: &Connection, directory: &str, limit: i64) -> anyhow::Result<Vec<SessionRow>> {
    session::Entity::find()
        .filter(listed())
        .filter(Expr::cust(NOT_PURGED))
        .filter(session::Column::Directory.eq(directory))
        .order_by_desc(session::Column::TimeUpdated)
        .order_by_desc(session::Column::Id)
        .limit(u64::try_from(limit).unwrap_or(u64::MAX))
        .all(&Db(conn))?
        .into_iter()
        .map(decode)
        .collect()
}

/// Node `softDeleteSession`：幂等，保留 message / part 与活动时间。
pub fn soft_delete(conn: &Connection, id: &str, time_deleted: i64) -> anyhow::Result<()> {
    session::Entity::update_many()
        .filter(session::Column::Id.eq(id))
        .col_expr(
            session::Column::TimeDeleted,
            Expr::expr(Func::coalesce([
                Expr::col(session::Column::TimeDeleted),
                Expr::val(time_deleted),
            ])),
        )
        .exec(&Db(conn))?;
    Ok(())
}

/// Node `touchSession`。
pub fn touch(conn: &Connection, id: &str, time_updated: i64) -> anyhow::Result<()> {
    session::Entity::update_many()
        .filter(session::Column::Id.eq(id))
        .col_expr(
            session::Column::TimeUpdated,
            Expr::expr(Func::cust(sea_orm::sea_query::Alias::new("max")).args([
                Expr::col(session::Column::TimeUpdated),
                Expr::val(time_updated),
            ])),
        )
        .exec(&Db(conn))?;
    Ok(())
}
