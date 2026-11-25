//! 借用当前 SQLite 连接；ORM 与旧 JSON 语句共享同一物理事务和回滚边界。
use rusqlite::Connection;
use sea_orm::{
    ConnectionTrait, DbBackend, DbErr, ExecResult, ProxyExecResult, ProxyRow, QueryResult,
    Statement,
};
use sea_query_rusqlite::{RusqliteValue, RusqliteValues};

pub(crate) struct Db<'a>(pub &'a Connection);

fn error(error: rusqlite::Error) -> DbErr {
    DbErr::Custom(error.to_string())
}
fn values(statement: &Statement) -> RusqliteValues {
    RusqliteValues(
        statement
            .values
            .as_ref()
            .map(|values| values.0.iter().cloned().map(RusqliteValue).collect())
            .unwrap_or_default(),
    )
}
impl ConnectionTrait for Db<'_> {
    fn get_database_backend(&self) -> DbBackend {
        DbBackend::Sqlite
    }
    fn execute_raw(&self, statement: Statement) -> Result<ExecResult, DbErr> {
        let values = values(&statement);
        let affected = self
            .0
            .prepare_cached(&statement.sql)
            .map_err(error)?
            .execute(values.as_params().as_slice())
            .map_err(error)?;
        Ok(ProxyExecResult {
            last_insert_id: self.0.last_insert_rowid() as u64,
            rows_affected: affected as u64,
        }
        .into())
    }
    fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        self.0.execute_batch(sql).map_err(error)?;
        Ok(ProxyExecResult {
            last_insert_id: self.0.last_insert_rowid() as u64,
            rows_affected: self.0.changes(),
        }
        .into())
    }
    fn query_one_raw(&self, statement: Statement) -> Result<Option<QueryResult>, DbErr> {
        Ok(self.query_all_raw(statement)?.into_iter().next())
    }
    fn query_all_raw(&self, statement: Statement) -> Result<Vec<QueryResult>, DbErr> {
        let values = values(&statement);
        let mut prepared = self.0.prepare_cached(&statement.sql).map_err(error)?;
        let columns: Vec<std::sync::Arc<str>> = prepared
            .column_names()
            .into_iter()
            .map(Into::into)
            .collect();
        prepared
            .query_map(values.as_params().as_slice(), |row| {
                // 逐列传播 SQLite 读取错误，不能把损坏的记录当成有效结果。
                let values = columns
                    .iter()
                    .enumerate()
                    .map(|(index, name)| {
                        let value = match row.get::<_, rusqlite::types::Value>(index)? {
                            rusqlite::types::Value::Null => sea_orm::Value::String(None),
                            rusqlite::types::Value::Integer(value) => value.into(),
                            rusqlite::types::Value::Real(value) => value.into(),
                            rusqlite::types::Value::Text(value) => value.into(),
                            rusqlite::types::Value::Blob(value) => value.into(),
                        };
                        Ok((name.to_string(), value))
                    })
                    .collect::<rusqlite::Result<std::collections::BTreeMap<_, _>>>()?;
                Ok(ProxyRow::from(values).into())
            })
            .map_err(error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::local_setting;
    use sea_orm::{EntityTrait, Set};
    #[test]
    fn orm_uses_the_callers_transaction_and_rolls_back_with_it() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table local_setting(scope text,scope_id text,namespace text,key text,value text not null,schema_version integer not null,time_created integer not null,time_updated integer not null,primary key(scope,scope_id,namespace,key))").unwrap();
        {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            local_setting::Entity::insert(local_setting::ActiveModel {
                scope: Set("directory".into()),
                scope_id: Set("/test".into()),
                namespace: Set("permission".into()),
                key: Set("rules".into()),
                value: Set("{}".into()),
                schema_version: Set(1),
                time_created: Set(1),
                time_updated: Set(1),
            })
            .exec(&Db(&tx))
            .unwrap();
            assert_eq!(
                local_setting::Entity::find().all(&Db(&tx)).unwrap().len(),
                1
            );
        }
        assert!(
            local_setting::Entity::find()
                .all(&Db(&conn))
                .unwrap()
                .is_empty()
        );
    }
}
