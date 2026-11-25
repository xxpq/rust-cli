//! 旧 local_setting 的四列复合键与首次写入时间保持不变。
use crate::{entities::local_setting, orm::Db};
use rusqlite::Connection;
use sea_orm::sea_query::OnConflict;
use sea_orm::{EntityTrait, Set};
pub fn read(
    conn: &Connection,
    scope: &str,
    id: &str,
    namespace: &str,
    key: &str,
) -> anyhow::Result<Option<String>> {
    Ok(local_setting::Entity::find_by_id((
        scope.to_owned(),
        id.to_owned(),
        namespace.to_owned(),
        key.to_owned(),
    ))
    .one(&Db(conn))?
    .map(|row| row.value))
}
pub fn write(
    conn: &Connection,
    scope: &str,
    id: &str,
    namespace: &str,
    key: &str,
    value: String,
    now: i64,
) -> anyhow::Result<()> {
    local_setting::Entity::insert(local_setting::ActiveModel {
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
    .exec(&Db(conn))?;
    Ok(())
}
