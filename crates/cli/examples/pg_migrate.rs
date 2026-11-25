//! 独立发布步骤；普通 CLI 与云端 Harness 启动不执行迁移。
use rust_cli_base::secret::RedactedString;

#[tokio::main]
#[expect(
    clippy::disallowed_methods,
    reason = "发布入口读取部署系统注入的管理员连接"
)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("ZCODE_SESSION_DB_MIGRATION_URL")
        .map_err(|_| "ZCODE_SESSION_DB_MIGRATION_URL is required")?;
    rust_cli_pg_store::migrate_url(&RedactedString::new(url)).await?;
    Ok(())
}
