//! 租户绑定的 PostgreSQL 会话存储（docs/specs/rust-cli/pg-store.md）。

mod access;
mod entities;
mod inputs;
mod orm;
mod rows;
mod rows_write;
mod schema;
mod session;
mod transaction;
mod usage;
mod usage_rows;

#[cfg(test)]
mod orm_tests;

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_policy::{
    BashInputCommit, DetachedAnswer, ResumeStart, SessionPolicy, SessionRulesRead,
};
use rust_cli_kernel_api::store::StoreError;
use rust_cli_node_records::ids::UuidSource;
use serde::Serialize;
use serde_json::Value;

/// 从连接池取连接最多等多久。
pub(crate) const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{ConnectOptions, PgPool};

pub use access::{StoreAccessContext, StoreProject};
pub use schema::{migrate, migrate_url};

/// 带 tenant_id 与 FORCE RLS 的表（Node 的 23 张业务表，pg-store.md §12）。
pub(crate) const TENANT_TABLES: [&str; 23] = [
    "session",
    "message",
    "part",
    "todo",
    "session_entry",
    "permission",
    "input_history",
    "local_setting",
    "session_target",
    "workflow_definition",
    "workflow_run",
    "workflow_activity",
    "workflow_event",
    "session_task_link",
    "model_usage",
    "turn_usage",
    "tool_usage",
    "session_input",
    "dwf_run",
    "dwf_actor",
    "dwf_node",
    "dwf_event",
    "zcode_database_identity",
];

/// 用这个会话库时会话的策略（kernel-api `session_policy`）：组装根在选会话库的那一处把它注入内核，内核只读这些值、
/// 不问会话库是哪一种。
///
/// - 恢复时等用户的下一条输入：账本里待处理的输入不自动执行（pg-store.md §4）；
/// - bash 输入在命令开始前提交：命令可能已有副作用，重开后不能再当成没执行过（§4）；
/// - 会话规则每次判决前现读：别的进程与子代理随时可能改（deployment.md §9）；
/// - 回答脱离了会话的询问前先占住会话表：这期间同一个会话不能被重新打开。
pub const SESSION_POLICY: SessionPolicy = SessionPolicy {
    resume: ResumeStart::OnNextInput,
    bash_input: BashInputCommit::BeforeRun,
    session_rules: SessionRulesRead::EachDecision,
    detached_answer: DetachedAnswer::HoldSession,
};

/// 每 Worker 共用一个池；bind 只创建身份视图，不创建数据库连接。
#[derive(Clone)]
pub struct PgDatabase {
    pool: PgPool,
}

impl PgDatabase {
    /// 仅供独立本机合同测试；生产入口只能使用带证书校验的 connect。
    #[cfg(any(test, feature = "testing"))]
    pub async fn connect_loopback_test(
        url: &RedactedString,
        max_connections: u32,
    ) -> Result<Self, StoreError> {
        let options = PgConnectOptions::from_str(url.expose())
            .map_err(|_| storage("invalid test PostgreSQL URL"))?;
        if options.get_host() != "127.0.0.1"
            || !options
                .get_database()
                .is_some_and(|name| name.ends_with("_test"))
        {
            return Err(storage(
                "test database must be an explicit loopback *_test database",
            ));
        }
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect_with(
                options
                    .ssl_mode(PgSslMode::Disable)
                    .disable_statement_logging(),
            )
            .await
            .map_err(database_error)?;
        Self::from_pool(pool).await
    }

    pub async fn connect(url: &RedactedString, max_connections: u32) -> Result<Self, StoreError> {
        Self::open(url, max_connections, true).await
    }

    /// CLI 内部测试入口（pg-store.md §11）：URL 写了 `sslmode` 时照它，没写时同 [`connect`](Self::connect)。
    pub async fn connect_configured(
        url: &RedactedString,
        max_connections: u32,
    ) -> Result<Self, StoreError> {
        Self::open(url, max_connections, !url.expose().contains("sslmode=")).await
    }

    async fn open(
        url: &RedactedString,
        max_connections: u32,
        verify_full: bool,
    ) -> Result<Self, StoreError> {
        if max_connections == 0 {
            return Err(storage("database connection budget must be positive"));
        }
        let mut options = PgConnectOptions::from_str(url.expose())
            .map_err(|_| storage("invalid PostgreSQL connection configuration"))?
            .disable_statement_logging();
        if verify_full {
            options = options.ssl_mode(PgSslMode::VerifyFull);
        }
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect_with(options)
            .await
            .map_err(database_error)?;
        Self::from_pool(pool).await
    }

    /// 已有池也必须验证业务角色与 schema；TLS 策略由创建池的可信组装根负责。
    pub async fn from_pool(pool: PgPool) -> Result<Self, StoreError> {
        let unsafe_role: bool = sqlx::query_scalar(
            "select exists(select 1 from pg_roles r where pg_has_role(current_user,r.oid,'MEMBER') and (r.rolsuper or r.rolbypassrls)) or exists(select 1 from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='zcode' and pg_has_role(current_user,c.relowner,'MEMBER')) or exists(select 1 from pg_namespace n where n.nspname='zcode' and pg_has_role(current_user,n.nspowner,'MEMBER'))"
        ).fetch_one(&pool).await.map_err(database_error)?;
        if unsafe_role {
            return Err(storage(
                "session database role must not own tables or bypass row security",
            ));
        }
        schema::check(&pool).await?;
        let protected: i64 = sqlx::query_scalar("select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='zcode' and c.relname=any($1) and c.relrowsecurity and c.relforcerowsecurity")
            .bind(TENANT_TABLES.to_vec())
            .fetch_one(&pool).await.map_err(database_error)?;
        if protected != TENANT_TABLES.len() as i64 {
            return Err(storage("session tables must enforce row security"));
        }
        Ok(Self { pool })
    }

    /// 只绑定可信租户和项目；会话路由与实例交接由宿主负责。
    pub fn bind(
        &self,
        access: StoreAccessContext,
        projects: Vec<StoreProject>,
        clock: Arc<dyn Clock>,
        uuids: Arc<dyn UuidSource>,
    ) -> Result<PgSessionStore, StoreError> {
        if projects.is_empty() || projects.iter().any(|p| p.id.trim().is_empty()) {
            return Err(storage("explicit project bindings are required"));
        }
        for (index, project) in projects.iter().enumerate() {
            if projects[..index].iter().any(|old| old.id == project.id) {
                return Err(storage("project bindings must be unique"));
            }
        }
        Ok(PgSessionStore {
            orm: sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(self.pool.clone()),
            access,
            projects: Arc::new(projects),
            clock,
            uuids,
        })
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
    pub fn connections(&self) -> u32 {
        self.pool.size()
    }
}

#[derive(Clone)]
pub struct PgSessionStore {
    orm: sea_orm::DatabaseConnection,
    access: StoreAccessContext,
    projects: Arc<Vec<StoreProject>>,
    clock: Arc<dyn Clock>,
    /// 记录 id 的随机部分：与本机的会话库同一个来源（node-records 的 `UuidSource`）。
    uuids: Arc<dyn UuidSource>,
}

#[cfg(feature = "testing")]
impl PgSessionStore {
    /// 对拍测试（pg-store.md §12.3）：本租户一张表的全部行（`row_to_json`），按行序或主键。
    pub async fn table_rows(&self, table: &str) -> Result<Vec<Value>, StoreError> {
        let order = match table {
            "message" | "part" | "session_entry" | "session_input" | "input_history" => "row_seq",
            "turn_usage" => "session_id, turn_id",
            "local_setting" => "scope, scope_id, namespace, key",
            "todo" => "session_id, position",
            _ => "id",
        };
        let sql = format!(
            "select row_to_json(t)::text as row from zcode.{table} t where tenant_id = $1 order by {order}"
        );
        let tx = self.begin(true).await?;
        let rows = crate::orm::native(&sql)
            .bind(self.tenant())
            .fetch_all(&tx)
            .await
            .map_err(crate::orm::error)?;
        tx.commit().await.map_err(crate::orm::error)?;
        rows.into_iter()
            .map(|row| {
                use sqlx::Row as _;
                serde_json::from_str(&row.get::<String, _>("row"))
                    .map_err(|_| storage("invalid row"))
            })
            .collect()
    }
}

impl PgSessionStore {
    fn tenant(&self) -> &str {
        self.access.tenant_key()
    }
    fn project_ids(&self) -> Vec<String> {
        self.projects.iter().map(|p| p.id.clone()).collect()
    }

    fn project(&self, workspace: &WorkspaceRef) -> Result<&StoreProject, StoreError> {
        let mut found = self
            .projects
            .iter()
            .filter(|project| project.workspace == *workspace);
        let project = found.next().ok_or_else(|| StoreError::Forbidden {
            operation: "unbound project workspace".into(),
        })?;
        if found.next().is_some() {
            return Err(storage(
                "select an explicit project binding before creating a session",
            ));
        }
        Ok(project)
    }
}

fn storage(message: impl Into<String>) -> StoreError {
    StoreError::Storage {
        message: message.into(),
    }
}
fn not_found(session: &SessionId) -> StoreError {
    StoreError::NotFound {
        session: session.clone(),
    }
}

// PG 的错误详情可能含约束值和正文，向调用方只给 SQLSTATE，不回传参数或 DSN。
fn database_error(error: sqlx::Error) -> StoreError {
    classify(&error)
}
pub(crate) fn classify(error: &sqlx::Error) -> StoreError {
    match error {
        sqlx::Error::Database(error) => storage(format!(
            "PostgreSQL error {}",
            error.code().as_deref().unwrap_or("unknown")
        )),
        sqlx::Error::RowNotFound => storage("database record missing"),
        sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => {
            storage("invalid stored record")
        }
        _ => StoreError::Unavailable {
            message: "database connection unavailable".into(),
        },
    }
}

fn encode(value: &impl Serialize) -> Result<Value, StoreError> {
    serde_json::to_value(value).map_err(|_| storage("cannot encode store record"))
}

fn number(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| storage("store integer out of range"))
}
fn unsigned(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| storage("negative store sequence or time"))
}

fn fingerprint(value: &impl Serialize) -> Result<String, StoreError> {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let ordered: std::collections::BTreeMap<_, _> = map
                    .into_iter()
                    .map(|(key, value)| (key, sorted(value)))
                    .collect();
                Value::Object(ordered.into_iter().collect())
            }
            Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
            other => other,
        }
    }
    let data = serde_json::to_vec(&sorted(encode(value)?))
        .map_err(|_| storage("cannot hash store request"))?;
    Ok(format!("{:x}", Sha256::digest(data)))
}

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
mod fault_tests;

#[cfg(test)]
mod load_tests;

#[cfg(test)]
mod security_tests;

#[cfg(test)]
mod facts_tests;
