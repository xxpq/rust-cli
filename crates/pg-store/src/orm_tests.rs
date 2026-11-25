//! 租户条件由适配器注入，不能依赖每个调用点手写过滤。
use sea_orm::{ActiveValue::Set, DbBackend, QueryTrait};

use crate::entities::local_setting;
use crate::tests::fixture;

fn setting(value: &str) -> local_setting::ActiveModel {
    local_setting::ActiveModel {
        scope: Set("project".into()),
        scope_id: Set("same-project".into()),
        namespace: Set("permission".into()),
        key: Set("mode".into()),
        value: Set(value.into()),
        schema_version: Set(1),
        time_created: Set(1),
        time_updated: Set(1),
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn tenant_scope_is_injected_and_foreign_insert_is_rejected() {
    let (_, store) = fixture("orm-scope").await;
    fn check<E: crate::entities::TenantEntity>(store: &crate::PgSessionStore) {
        for query in [
            store.find::<E>().build(DbBackend::Postgres),
            store.update::<E>().build(DbBackend::Postgres),
            store.delete_rows::<E>().build(DbBackend::Postgres),
        ] {
            assert!(query.sql.contains("tenant_id"));
            assert_eq!(query.values.unwrap().0, vec![store.tenant().into()]);
        }
    }
    use crate::entities::*;
    check::<session::Entity>(&store);
    check::<message::Entity>(&store);
    check::<part::Entity>(&store);
    check::<session_entry::Entity>(&store);
    check::<session_input::Entity>(&store);
    check::<local_setting::Entity>(&store);
    check::<permission::Entity>(&store);
    check::<session_target::Entity>(&store);
    check::<model_usage::Entity>(&store);
    check::<tool_usage::Entity>(&store);
    check::<turn_usage::Entity>(&store);
    let own = store.scoped(setting("own")).unwrap();
    assert_eq!(own.tenant_id, Set(store.tenant().to_owned()));
    let foreign = local_setting::ActiveModel {
        tenant_id: Set("different-tenant".to_owned()),
        ..setting("foreign")
    };
    assert!(matches!(
        store.scoped(foreign),
        Err(rust_cli_kernel_api::store::StoreError::Forbidden { .. })
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn orm_crud_and_upserts_inject_tenant_on_a_shared_pool() {
    use sea_orm::{
        ColumnTrait, EntityTrait, QueryFilter,
        sea_query::{Expr, OnConflict},
    };
    let (_, first) = fixture("orm-first").await;
    let (_, mut second) = fixture("orm-second").await;
    second.orm = first.orm.clone();
    for (store, value) in [(&first, "1"), (&second, "2"), (&first, "3")] {
        let tx = store.begin(false).await.unwrap();
        local_setting::Entity::insert(store.scoped(setting(value)).unwrap())
            .on_conflict(
                OnConflict::columns([
                    local_setting::Column::TenantId,
                    local_setting::Column::Scope,
                    local_setting::Column::ScopeId,
                    local_setting::Column::Namespace,
                    local_setting::Column::Key,
                ])
                .update_column(local_setting::Column::Value)
                .to_owned(),
            )
            .exec(&tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let tx = first.begin(false).await.unwrap();
    let updated = first
        .update::<local_setting::Entity>()
        .col_expr(local_setting::Column::Value, Expr::val("4"))
        .exec(&tx)
        .await
        .unwrap();
    assert_eq!(updated.rows_affected, 1);
    let value = first
        .find::<local_setting::Entity>()
        .one(&tx)
        .await
        .unwrap()
        .unwrap()
        .value;
    assert_eq!(value, "4");
    let removed = first
        .delete_rows::<local_setting::Entity>()
        .exec(&tx)
        .await
        .unwrap();
    assert_eq!(removed.rows_affected, 1);
    tx.commit().await.unwrap();
    let tx = second.begin(true).await.unwrap();
    let rows = second
        .find::<local_setting::Entity>()
        .filter(local_setting::Column::ScopeId.eq("same-project"))
        .all(&tx)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value, "2");
    tx.commit().await.unwrap();
}

// CR-01 回归：ORM 业务语句阶段的断线必须是 Unavailable，Actor 才会暂停写入并等待重新打开。
#[test]
fn connection_failures_are_unavailable_not_storage() {
    use rust_cli_kernel_api::store::StoreError;
    use sea_orm::{ConnAcquireErr, DbErr, RuntimeErr};
    use std::sync::Arc;
    let sqlx = |error: sqlx::Error| DbErr::Exec(RuntimeErr::SqlxError(Arc::new(error)));
    for error in [
        sqlx(sqlx::Error::Io(std::io::ErrorKind::ConnectionReset.into())),
        sqlx(sqlx::Error::PoolTimedOut),
        DbErr::Query(RuntimeErr::SqlxError(Arc::new(sqlx::Error::Protocol(
            "unexpected message".into(),
        )))),
        DbErr::ConnectionAcquire(ConnAcquireErr::Timeout),
        DbErr::ConnectionAcquire(ConnAcquireErr::ConnectionClosed),
    ] {
        assert!(
            matches!(crate::orm::error(error), StoreError::Unavailable { .. }),
            "connection failures must pause the session"
        );
    }
    assert!(matches!(
        crate::orm::error(sqlx(sqlx::Error::RowNotFound)),
        StoreError::Storage { .. }
    ));
    assert!(matches!(
        crate::orm::error(DbErr::RecordNotInserted),
        StoreError::Storage { .. }
    ));
}
