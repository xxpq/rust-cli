//! 工作区 MCP 的信任记录 `workspace-mcp-trust-v1.json`（docs/specs/rust-cli/ext.md §3.1 与 18-mcp-trust.md §12.3），与 Node CLI、App 共用。
//!
//! 格式与校验同 Node `apps/zcode-cli/packages/bootstrap/src/workspace-mcp-trust/index.ts` 的 `storeSchema`（strict：多余的键、缺字段、
//! 格式不对都让整份记录作废，fail closed）；写入遵守 Node 的目录锁（`withFileLock`）与私有原子写（`atomicWritePrivateTextFile`）。
//! 读取不加锁（写入是原子 rename，读到的总是完整的一版）。

use rust_cli_fs::{FileMode, LockOptions, ParentDir, TempName, WriteOptions};
use rust_cli_paths::StoragePath;
use serde_json::{Map, Value, json};

use super::hook_trust::{datetime, sha256_hex};

/// 文件的版本（Node `schemaVersion: z.literal(1)`）。
const SCHEMA_VERSION: f64 = 1.0;
/// 文件顶层的键与一条记录的键（strict：多一个、少一个都作废）。
const STORE_KEYS: [&str; 2] = ["schemaVersion", "records"];
const RECORD_KEYS: [&str; 4] = [
    "workspaceIdentity",
    "serverName",
    "declarationDigest",
    "grantedAt",
];

/// 私有原子写：同目录隐藏的临时文件、0600、rename 重试（Node `atomicWritePrivateTextFile`）。
const PRIVATE_WRITE: WriteOptions = WriteOptions {
    mode: FileMode::Private,
    temp_name: TempName::Hidden,
    parent: ParentDir::Existing,
    rename_retry: rust_cli_fs::NODE_RENAME_RETRY,
};

/// 记录的键：一个工作区里一个服务器的一份声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpTrustKey<'a> {
    pub identity: &'a str,
    pub server: &'a str,
    pub digest: &'a str,
}

/// 一条记录（Node `recordSchema`，字段顺序即写出的键序）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTrustRecord {
    pub workspace_identity: String,
    pub server_name: String,
    pub declaration_digest: String,
    pub granted_at: String,
}

impl McpTrustRecord {
    fn parse(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.len() != RECORD_KEYS.len()
            || RECORD_KEYS.iter().any(|key| !object.contains_key(*key))
        {
            return None;
        }
        let text = |key: &str, valid: &dyn Fn(&str) -> bool| {
            object
                .get(key)
                .and_then(Value::as_str)
                .filter(|text| valid(text))
                .map(str::to_owned)
        };
        let non_empty = |text: &str| !text.is_empty();
        Some(Self {
            workspace_identity: text("workspaceIdentity", &non_empty)?,
            server_name: text("serverName", &non_empty)?,
            declaration_digest: text("declarationDigest", &sha256_hex)?,
            granted_at: text("grantedAt", &datetime)?,
        })
    }

    fn to_value(&self) -> Value {
        json!({
            "workspaceIdentity": self.workspace_identity,
            "serverName": self.server_name,
            "declarationDigest": self.declaration_digest,
            "grantedAt": self.granted_at,
        })
    }

    fn matches(&self, key: &McpTrustKey<'_>) -> bool {
        self.workspace_identity == key.identity
            && self.server_name == key.server
            && self.declaration_digest == key.digest
    }
}

/// 读取的结果。文件不存在是没有记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTrustLoad {
    Records(Vec<McpTrustRecord>),
    /// 读不出或不合法：全部视为未信任。
    Corrupt,
}

impl McpTrustLoad {
    /// 这份声明有没有信任记录。
    pub fn trusts(&self, key: &McpTrustKey<'_>) -> bool {
        match self {
            Self::Records(records) => records.iter().any(|record| record.matches(key)),
            Self::Corrupt => false,
        }
    }
}

/// `decide` 的失败（Node `decide` 抛出的两类，协议上分别是 `precondition.mcpDeclarationChanged` 与 `precondition.mcpTrustUnavailable`）。
#[derive(Debug, thiserror::Error)]
pub enum McpTrustError {
    /// 锁内重读之后没有这个声明，或 digest 不同。
    #[error("workspace_mcp_declaration_changed")]
    DeclarationChanged,
    /// 项目配置读不出：没有可以批准的声明。
    #[error("the workspace MCP configuration cannot be read")]
    ConfigUnreadable,
    /// 记录文件坏了：不覆盖。
    #[error("the workspace MCP trust store is corrupt")]
    Corrupt,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Node `storeSchema`：解析整份文件；任何不合法都返回 None。
fn parse_store(bytes: &[u8]) -> Option<Vec<McpTrustRecord>> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    // `z.literal(1)`：JSON 的 `1.0` 解出来也是 1
    let fixed = object.len() == STORE_KEYS.len()
        && object.get("schemaVersion").and_then(Value::as_f64) == Some(SCHEMA_VERSION);
    if !fixed {
        return None;
    }
    object
        .get("records")?
        .as_array()?
        .iter()
        .map(McpTrustRecord::parse)
        .collect()
}

/// 记录文件。
#[derive(Debug, Clone)]
pub struct McpTrustStore {
    file: StoragePath,
}

impl McpTrustStore {
    /// `file` 来自 `StoragePaths::mcp_trust_store`。
    pub fn new(file: StoragePath) -> Self {
        Self { file }
    }

    pub fn path(&self) -> &StoragePath {
        &self.file
    }

    /// 读当前记录（不加锁）。
    pub async fn load(&self) -> McpTrustLoad {
        match rust_cli_fs::read_optional(self.file.as_path()).await {
            Ok(None) => McpTrustLoad::Records(Vec::new()),
            Ok(Some(bytes)) => {
                parse_store(&bytes).map_or(McpTrustLoad::Corrupt, McpTrustLoad::Records)
            }
            Err(_) => McpTrustLoad::Corrupt,
        }
    }

    /// 信任或撤销一份声明（Node `decide`）：0700 目录 → 目录锁 → `verify`（调用方按磁盘上的声明重算 digest，对不上就报错）→
    /// 重读记录（坏了报错、不覆盖）→ 去掉这一份、`trusted` 时追加 → 原子写。要的结果已是现状时不写，返回假（设值语义，18 §12.3）。
    pub async fn decide(
        &self,
        key: &McpTrustKey<'_>,
        trusted: bool,
        granted_at: &str,
        verify: impl AsyncFnOnce() -> Result<(), McpTrustError>,
    ) -> Result<bool, McpTrustError> {
        let file = self.file.as_path();
        if let Some(parent) = file.parent() {
            rust_cli_fs::create_private_dir_all(parent).await?;
        }
        rust_cli_fs::with_file_lock(file, &LockOptions::default(), async || {
            verify().await?;
            let current = match rust_cli_fs::read_optional(file).await? {
                None => Vec::new(),
                Some(bytes) => parse_store(&bytes).ok_or(McpTrustError::Corrupt)?,
            };
            if current.iter().any(|record| record.matches(key)) == trusted {
                return Ok(false);
            }
            let mut next: Vec<McpTrustRecord> = current
                .into_iter()
                .filter(|record| !record.matches(key))
                .collect();
            if trusted {
                next.push(McpTrustRecord {
                    workspace_identity: key.identity.to_owned(),
                    server_name: key.server.to_owned(),
                    declaration_digest: key.digest.to_owned(),
                    granted_at: granted_at.to_owned(),
                });
            }
            let mut store = Map::new();
            store.insert("schemaVersion".into(), json!(1));
            store.insert(
                "records".into(),
                Value::Array(next.iter().map(McpTrustRecord::to_value).collect()),
            );
            // Node 写 `JSON.stringify({schemaVersion: 1, records})`：紧凑、没有结尾的换行
            let text = rust_cli_base::js::json::stringify(&Value::Object(store));
            rust_cli_fs::atomic_write(file, text.as_bytes(), PRIVATE_WRITE).await?;
            Ok(true)
        })
        .await
    }
}

#[cfg(test)]
#[path = "mcp_trust_tests.rs"]
mod tests;
