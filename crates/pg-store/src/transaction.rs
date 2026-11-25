use crate::orm::error as orm_error;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::StoreError;
use sea_orm::TransactionTrait;
use sqlx::Row;

/// 等连接超过这么久记一条告警。
const POOL_WAIT_WARN: std::time::Duration = std::time::Duration::from_millis(100);

use crate::{PgSessionStore, not_found, unsigned};

pub(crate) type Tx = sea_orm::DatabaseTransaction;

impl PgSessionStore {
    pub(crate) async fn begin(&self, snapshot: bool) -> Result<Tx, StoreError> {
        let waiting = std::time::Instant::now();
        let tx = if snapshot {
            self.orm
                .begin_with_config(
                    Some(sea_orm::IsolationLevel::RepeatableRead),
                    Some(sea_orm::AccessMode::ReadOnly),
                )
                .await
        } else {
            self.orm.begin().await
        }
        .map_err(orm_error)?;
        if waiting.elapsed() > POOL_WAIT_WARN {
            tracing::warn!(
                event = "store.pool_wait",
                wait_ms = waiting.elapsed().as_millis() as u64,
                "session database pool pressure"
            );
        }
        crate::orm::native("select set_config('zcode.tenant_id',$1,true),set_config('statement_timeout','10000',true),set_config('lock_timeout','5000',true),set_config('idle_in_transaction_session_timeout','10000',true)")
            .bind(self.tenant()).execute(&tx).await.map_err(orm_error)?;
        let now: i64 =
            crate::orm::scalar("select (extract(epoch from clock_timestamp())*1000)::bigint")
                .fetch_one(&tx)
                .await
                .map_err(orm_error)?;
        self.access.check(unsigned(now)?)?;
        Ok(tx)
    }

    pub(crate) async fn live(
        &self,
        tx: &mut Tx,
        session: &SessionId,
        lock: bool,
    ) -> Result<(), StoreError> {
        use crate::entities::session;
        use sea_orm::{ColumnTrait, QueryFilter, QuerySelect};
        let mut query = self
            .find::<session::Entity>()
            .filter(session::Column::Id.eq(session.as_str()))
            .filter(session::Column::TimeDeleted.is_null());
        if lock {
            query = query.lock_exclusive();
        }
        let row = crate::orm::select(query)
            .fetch_optional(&*tx)
            .await
            .map_err(orm_error)?
            .ok_or_else(|| not_found(session))?;
        if !self
            .projects
            .iter()
            .any(|project| project.id == row.get::<String, _>("project_id"))
        {
            return Err(not_found(session));
        }
        Ok(())
    }

    /// COMMIT 回包丢失不能证明回滚，调用方停止写入，重新打开后读取已提交的状态。
    pub(crate) async fn finish(&self, tx: Tx) -> Result<(), StoreError> {
        tx.commit().await.map_err(|_| StoreError::OutcomeUnknown)
    }
}
