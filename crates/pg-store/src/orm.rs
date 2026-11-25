//! ORM 的租户入口与少量数据库专用语句的执行桥；两者共用同一事务。
use std::marker::PhantomData;
use std::ops::Deref;

use rust_cli_kernel_api::store::StoreError;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend,
    DbErr, EntityTrait, ExecResult, QueryFilter, QueryResult, RuntimeErr, Select, Statement,
    TryGetable, Value,
};

use crate::{PgSessionStore, entities::TenantEntity};

impl PgSessionStore {
    pub(crate) fn find<E: TenantEntity>(&self) -> Select<E> {
        E::find().filter(E::tenant_column().eq(self.tenant()))
    }

    pub(crate) fn scoped<A: ActiveModelTrait>(&self, mut model: A) -> Result<A, StoreError>
    where
        A::Entity: TenantEntity,
    {
        let column = A::Entity::tenant_column();
        let tenant: Value = self.tenant().into();
        match model.get(column) {
            ActiveValue::Set(value) | ActiveValue::Unchanged(value) if value != tenant => {
                return Err(StoreError::Forbidden {
                    operation: "cross-tenant ORM write".into(),
                });
            }
            _ => {}
        }
        model.set(column, tenant);
        Ok(model)
    }

    pub(crate) fn update<E: TenantEntity>(&self) -> sea_orm::UpdateMany<E> {
        E::update_many().filter(E::tenant_column().eq(self.tenant()))
    }

    pub(crate) fn delete_rows<E: TenantEntity>(&self) -> sea_orm::DeleteMany<E> {
        E::delete_many().filter(E::tenant_column().eq(self.tenant()))
    }
}

// 修复 CR-01：ORM 重构后断线、池超时等连接错误被归成 Storage，Actor 不会暂停写入，
// 已接纳输入留在 pending 时后续输入仍会执行。这里复用 SQLx 路径的同一分类，连接类错误返回 Unavailable。
pub(crate) fn error(error: DbErr) -> StoreError {
    match error {
        DbErr::Exec(RuntimeErr::SqlxError(error))
        | DbErr::Conn(RuntimeErr::SqlxError(error))
        | DbErr::Query(RuntimeErr::SqlxError(error)) => crate::classify(&error),
        DbErr::ConnectionAcquire(_) => StoreError::Unavailable {
            message: "database connection unavailable".into(),
        },
        _ => crate::storage("session ORM operation failed"),
    }
}

/// 固定 PG 驱动，保留数据库专用聚合结果的类型解码。
pub(crate) struct Row(QueryResult);
impl Deref for Row {
    type Target = sqlx::postgres::PgRow;
    #[allow(clippy::expect_used, reason = "此模块只由 PostgreSQL 连接构造查询结果")]
    fn deref(&self) -> &Self::Target {
        self.0.try_as_pg_row().expect("PostgreSQL ORM result")
    }
}

pub(crate) struct NativeQuery {
    sql: String,
    values: Vec<Value>,
}
pub(crate) fn native(sql: &str) -> NativeQuery {
    NativeQuery {
        sql: sql.into(),
        values: Vec::new(),
    }
}
pub(crate) fn select<E: EntityTrait>(query: Select<E>) -> NativeQuery {
    use sea_orm::QueryTrait;
    let statement = query.build(DbBackend::Postgres);
    NativeQuery {
        sql: statement.sql,
        values: statement.values.map_or_else(Vec::new, |values| values.0),
    }
}
impl NativeQuery {
    pub(crate) fn bind(mut self, value: impl Into<Value>) -> Self {
        self.values.push(value.into());
        self
    }
    fn statement(self) -> Statement {
        Statement::from_sql_and_values(DbBackend::Postgres, self.sql, self.values)
    }
    pub(crate) async fn execute(self, tx: &DatabaseTransaction) -> Result<ExecResult, DbErr> {
        tx.execute_raw(self.statement()).await
    }
    pub(crate) async fn fetch_all(self, tx: &DatabaseTransaction) -> Result<Vec<Row>, DbErr> {
        tx.query_all_raw(self.statement())
            .await
            .map(|rows| rows.into_iter().map(Row).collect())
    }
    pub(crate) async fn fetch_optional(
        self,
        tx: &DatabaseTransaction,
    ) -> Result<Option<Row>, DbErr> {
        tx.query_one_raw(self.statement())
            .await
            .map(|row| row.map(Row))
    }
    pub(crate) async fn fetch_one(self, tx: &DatabaseTransaction) -> Result<Row, DbErr> {
        self.fetch_optional(tx)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("session row".into()))
    }
}

pub(crate) struct NativeScalar<T>(NativeQuery, PhantomData<T>);
pub(crate) fn scalar<T: TryGetable>(sql: &str) -> NativeScalar<T> {
    NativeScalar(native(sql), PhantomData)
}
impl<T: TryGetable> NativeScalar<T> {
    pub(crate) async fn fetch_one(self, tx: &DatabaseTransaction) -> Result<T, DbErr> {
        self.0.fetch_one(tx).await?.0.try_get_by_index(0)
    }
}
