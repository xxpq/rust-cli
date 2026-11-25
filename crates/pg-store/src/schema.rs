use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

use crate::{database_error, storage};
use rust_cli_kernel_api::store::StoreError;

const SCHEMA: &str = rust_cli_base::include_lf!("../migrations/0001_sessions.sql");
const VERSION: i32 = 1;

/// 只由发布入口使用管理员池执行；业务启动只调用 check。
pub async fn migrate(pool: &PgPool) -> Result<(), StoreError> {
    let mut tx = pool.begin().await.map_err(database_error)?;
    sqlx::query("select pg_advisory_xact_lock(7831062841)")
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    let exists: bool = sqlx::query_scalar("select to_regclass('zcode.schema_version') is not null")
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
    if exists {
        let rows =
            sqlx::query("select version,checksum from zcode.schema_version order by version")
                .fetch_all(&mut *tx)
                .await
                .map_err(database_error)?;
        if rows.len() != 1
            || rows[0].get::<i32, _>("version") != VERSION
            || rows[0].get::<String, _>("checksum") != checksum()
        {
            return Err(storage("unsupported or modified session schema migration"));
        }
    } else {
        sqlx::raw_sql(SCHEMA)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        sqlx::query("insert into zcode.schema_version(version,checksum) values($1,$2)")
            .bind(VERSION)
            .bind(checksum())
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
    }
    tx.commit().await.map_err(database_error)
}

fn checksum() -> String {
    format!("{:x}", Sha256::digest(SCHEMA.as_bytes()))
}

pub(crate) async fn check(pool: &PgPool) -> Result<(), StoreError> {
    let rows = sqlx::query("select version,checksum from zcode.schema_version order by version")
        .fetch_all(pool)
        .await
        .map_err(database_error)?;
    if rows.len() != 1
        || rows[0].get::<i32, _>("version") != VERSION
        || rows[0].get::<String, _>("checksum") != checksum()
    {
        return Err(storage(
            "session schema must be migrated by the release process",
        ));
    }
    Ok(())
}

/// 发布作业入口；独立管理员连接，不向运行中的会话池授予 DDL 权限。
pub async fn migrate_url(url: &rust_cli_base::secret::RedactedString) -> Result<(), StoreError> {
    use sqlx::ConnectOptions;
    use std::str::FromStr;
    let options = sqlx::postgres::PgConnectOptions::from_str(url.expose())
        .map_err(|_| storage("invalid migration connection configuration"))?
        .ssl_mode(sqlx::postgres::PgSslMode::VerifyFull)
        .disable_statement_logging();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(crate::ACQUIRE_TIMEOUT)
        .connect_with(options)
        .await
        .map_err(database_error)?;
    let result = migrate(&pool).await;
    pool.close().await;
    result
}
