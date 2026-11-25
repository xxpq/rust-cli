use std::sync::Arc;

use crate::{StoreAccessContext, StoreProject};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId, TraceId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::store::{
    Fact, FactBatch, SessionKind, SessionMeta, SessionStore, StoreError,
};
use rust_cli_kernel_api::testing::FixedClock;
use sqlx::postgres::PgPoolOptions;

use super::*;

/// 测试用的记录 uuid：顺序值（同 kernel-api 的 `SequentialIds` 的写法）。
#[derive(Default)]
pub(super) struct SequentialUuids(std::sync::atomic::AtomicU64);

impl UuidSource for SequentialUuids {
    fn uuid(&self) -> String {
        let next = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        format!("uuid_{next}")
    }
}

pub(super) async fn fixture(label: &str) -> (PgDatabase, PgSessionStore) {
    let (database, store, _) = tenant_fixture(label).await;
    (database, store)
}

#[expect(
    clippy::disallowed_methods,
    reason = "独立 PG 测试读取专用连接与唯一租户时间"
)]
pub(super) async fn tenant_fixture(label: &str) -> (PgDatabase, PgSessionStore, String) {
    let url = std::env::var("ZCODE_TEST_POSTGRES_URL").expect("set the isolated test database URL");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let database = PgDatabase::from_pool(pool).await.unwrap();
    let tenant = format!(
        "{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let access = StoreAccessContext::verified(&tenant, u64::MAX).unwrap();
    let store = database
        .bind(
            access,
            vec![project()],
            Arc::new(FixedClock(1_000)),
            Arc::new(SequentialUuids::default()),
        )
        .unwrap();
    (database, store, tenant)
}

pub(super) fn project() -> StoreProject {
    StoreProject {
        id: "project".into(),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("sandbox"),
            root: EnvPath::new("/workspace"),
        },
    }
}

pub(super) fn meta(id: &str) -> SessionMeta {
    SessionMeta {
        id: SessionId::new(id),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: project().workspace,
        title: "original".into(),
        trace: TraceId::new(id),
        created_at_ms: 1_000,
    }
}

pub(super) fn batch(id: &str) -> FactBatch {
    FactBatch {
        command: None,
        turn: None,
        facts: vec![Fact::MessageAppended {
            message: Message::User(UserMessage {
                id: rust_cli_kernel_api::ids::MessageId::new(id),
                content: vec![ContentBlock::Text {
                    text: "hello".into(),
                }],
                origin: UserOrigin::Human,
            }),
        }],
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run scripts/rust-cli/pg-store.mjs"]
async fn session_write_needs_no_write_id_and_hides_other_tenants() {
    let (database, store) = fixture("contract-a").await;
    let session = SessionId::new("one");
    store.create(meta("one"), batch("m1")).await.unwrap();
    store.commit(&session, batch("m2")).await.unwrap();
    assert_eq!(store.load(&session).await.unwrap().messages.len(), 2);
    let access = StoreAccessContext::verified("contract-b", u64::MAX).unwrap();
    let other = database
        .bind(
            access,
            vec![project()],
            Arc::new(FixedClock(1_000)),
            Arc::new(SequentialUuids::default()),
        )
        .unwrap();
    assert!(matches!(
        other.load(&session).await,
        Err(StoreError::NotFound { .. })
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; run scripts/rust-cli/pg-store.mjs"]
async fn legacy_worker_columns_are_inert_even_after_soft_delete() {
    let (_, store) = fixture("legacy-columns").await;
    let session = SessionId::new("one");
    store.create(meta("one"), batch("first")).await.unwrap();
    let tx = store.begin(false).await.unwrap();
    // v1 旧列必须原样保留；用仍有效的旧值验证读写、删除均不会依赖或清理它们。
    let empty: bool = crate::orm::scalar("select owner_id is null and lease_until is null and owner_epoch = 0 from zcode.session where id = 'one'").fetch_one(&tx).await.unwrap();
    assert!(empty);
    crate::orm::native("update zcode.session set owner_id = 'previous', owner_epoch = 7, lease_until = 'infinity' where id = 'one'").execute(&tx).await.unwrap();
    tx.commit().await.unwrap();
    store.commit(&session, batch("second")).await.unwrap();
    store.delete(&session).await.unwrap();
    let tx = store.begin(true).await.unwrap();
    let unchanged: bool = crate::orm::scalar("select owner_id = 'previous' and owner_epoch = 7 and lease_until = 'infinity' from zcode.session where id = 'one'").fetch_one(&tx).await.unwrap();
    assert!(unchanged);
}

#[tokio::test]
#[ignore = "requires a disposable local test database administrator"]
#[expect(clippy::disallowed_methods, reason = "独立 PG 测试读取专用管理员连接")]
async fn initialize_test_database() {
    let url =
        std::env::var("ZCODE_TEST_POSTGRES_ADMIN_URL").expect("isolated test administrator URL");
    assert!(
        url.starts_with("postgres://postgres@127.0.0.1:") && url.ends_with("/zcode_store_test")
    );
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    migrate(&pool).await.unwrap();
    migrate(&pool).await.unwrap();
    let tables = crate::TENANT_TABLES
        .map(|name| format!("zcode.{name}"))
        .join(",");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("do $$ begin if not exists(select 1 from pg_roles where rolname='zcode_session_test') then create role zcode_session_test login nosuperuser nobypassrls; end if; end $$; grant usage on schema zcode to zcode_session_test; grant select on zcode.schema_version to zcode_session_test; grant select,insert,update,delete on {tables} to zcode_session_test;")))
        .execute(&pool).await.unwrap();
    assert!(
        PgDatabase::from_pool(pool.clone()).await.is_err(),
        "administrator must not be usable as the runtime role"
    );
    pool.close().await;
}
