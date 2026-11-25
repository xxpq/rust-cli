//! 凭据密钥的来源（docs/specs/rust-cli/platform.md §13.2、§13.3、§13.5）。v2 文件与新文件：显式受管密钥；否则在
//! `<文件>.keyring` 的目录锁里看固定的来源、凭据目录的 `credential-secret` 与系统凭据库，结果（成功与失败）缓存到进程
//! 结束，只有没拿到 keyring 锁的失败不缓存。v1 文件（[`KeySource::legacy`]）：只看受管密钥与 `credential-secret`，
//! 不碰系统凭据库。一个凭据文件一份 [`KeySource`]。

use std::io;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use rust_cli_base::proto::mode::GROUP_OTHER_BITS;
use rust_cli_base::secret::RedactedString;
use rust_cli_fs::LockOptions;
use serde_json::{Map, Value};
use tokio::sync::OnceCell;

use super::format::{KeyMaterial, LegacyKeys, Source, V2_PREFIX, digest, legacy_keys};
use super::seal::{BASE64URL, KEY_BYTES, Key, random_bytes};
use super::{CredentialError, CredentialSecret, PRIVATE_WRITE};

/// 系统凭据库（macOS 钥匙串、Windows 凭据管理器、Linux Secret Service）的端口：Node `CredentialKeyStore` 加上条目名。
/// 条目名由平台层算（[`SERVICE`] 与凭据文件的账号名），实现只管存取。错误是给日志的原因，平台层一律当作不可用。
#[async_trait]
pub trait SystemKeyStore: Send + Sync {
    /// 条目的值；没有这个条目时为 `None`。
    async fn read(&self, service: &str, account: &str) -> Result<Option<RedactedString>, String>;
    /// 写条目：已有就换值，没有就建。
    async fn write(
        &self,
        service: &str,
        account: &str,
        value: &RedactedString,
    ) -> Result<(), String>;
}

/// 系统凭据库里的服务名（Node `SYSTEM_SERVICE`）。
pub(crate) const SERVICE: &str = "ZCode Credential Encryption";
/// 凭据目录里固定来源的文件；内容只能是 [`PINNED_OS`]。
const SOURCE_FILE: &str = "credential-source";
const PINNED_OS: &[u8] = b"os\n";
/// 凭据目录里的缺省受管密钥文件。
const SECRET_FILE: &str = "credential-secret";
/// keyring 锁的路径：凭据文件名加这个后缀（锁目录 `credentials.json.keyring.lock/`）。
const KEYRING_LOCK_SUFFIX: &str = ".keyring";
/// 系统凭据库里的密钥：32 字节的 base64url，无填充 43 个字符。
const ENCODED_KEY_CHARS: usize = 43;
/// 受管密钥的字节数（UTF-8）。
const MANAGED_SECRET_BYTES: RangeInclusive<usize> = 32..=4096;

/// 系统凭据库里这个凭据文件的账号名：sha256(绝对路径，Node `path.resolve` 的写法) 的 hex。
pub(crate) fn account_of(file: &Path) -> String {
    let resolved = rust_cli_base::path::normalize(file);
    rust_cli_base::hash::sha256_hex(resolved.to_string_lossy().as_bytes())
}

/// 不可用的原因（只进日志）。
type Refusal = String;

/// 一个凭据文件的密钥来源与进程内缓存。
pub(crate) struct KeySource {
    file: PathBuf,
    account: String,
    secret: CredentialSecret,
    store: Arc<dyn SystemKeyStore>,
    /// 显式受管密钥（环境变量与它指的文件）的原值：每个进程读一次，失败也留着。
    managed: OnceCell<Result<Option<RedactedString>, CredentialError>>,
    /// 系统路径的结果；外层的错误（没拿到 keyring 锁）不进缓存。
    system: OnceCell<Result<Arc<KeyMaterial>, CredentialError>>,
}

impl KeySource {
    pub(crate) fn new(
        file: &Path,
        secret: &CredentialSecret,
        store: Arc<dyn SystemKeyStore>,
    ) -> Self {
        Self {
            file: file.to_path_buf(),
            account: account_of(file),
            secret: secret.clone(),
            store,
            managed: OnceCell::new(),
            system: OnceCell::new(),
        }
    }

    /// v2 文件与新文件的密钥。`create`：系统凭据库里没有密钥时可以新建（写新文件时可以，读 v2 文件时不可以）。
    pub(crate) async fn key(&self, create: bool) -> Result<Arc<KeyMaterial>, CredentialError> {
        if let Some(secret) = self.explicit_secret().await? {
            return Ok(self.env_key(secret.expose()));
        }
        match self
            .system
            .get_or_try_init(|| self.resolve_system(create))
            .await
        {
            Ok(outcome) => outcome.clone(),
            Err(not_locked) => Err(not_locked),
        }
    }

    /// 只有 v1 与明文的文件的密钥（platform.md §13.2 第 4 条）：不碰系统凭据库，也不拿 keyring 锁（不建密钥）。
    /// secret 是显式受管密钥；没有时，没有固定来源的话是凭据目录的 `credential-secret`。写入用的密钥按文件里的旧值选。
    pub(crate) async fn legacy(
        &self,
        record: &Map<String, Value>,
    ) -> Result<LegacyKeys, CredentialError> {
        let fallback = self.secret.fallback();
        let values = record.values().filter_map(Value::as_str);
        if let Some(secret) = self.explicit_secret().await? {
            let candidates = legacy_keys(Some(secret.expose()), &fallback);
            return Ok(LegacyKeys::new(candidates, digest(secret.expose()), values));
        }
        let discovered = self.discovered().await.map_err(refuse)?;
        let candidates = legacy_keys(discovered.as_deref(), &fallback);
        Ok(LegacyKeys::new(candidates, digest(&fallback), values))
    }

    /// 显式受管密钥的原值（每个进程读一次）。
    async fn explicit_secret(&self) -> Result<Option<RedactedString>, CredentialError> {
        self.managed.get_or_init(|| self.explicit()).await.clone()
    }

    /// 没有显式受管密钥时，v1 的候选密钥里的 secret：固定了来源（`credential-source`）就没有（Node 的来源是 `os` 时
    /// 只有缺省派生）；否则是缺省受管文件 `credential-secret`。
    async fn discovered(&self) -> Result<Option<String>, Refusal> {
        if self.pinned().await? {
            return Ok(None);
        }
        managed_file(&self.file.with_file_name(SECRET_FILE), Discovery::Default).await
    }

    /// 来源固定在系统凭据库（`credential-source` 是 `os\n`）；内容是别的值时不可用。
    async fn pinned(&self) -> Result<bool, Refusal> {
        match read(&self.file.with_file_name(SOURCE_FILE)).await? {
            None => Ok(false),
            Some(content) if content == PINNED_OS => Ok(true),
            Some(_) => Err(format!("{SOURCE_FILE} does not name the system store")),
        }
    }

    fn env_key(&self, secret: &str) -> Arc<KeyMaterial> {
        let legacy = legacy_keys(Some(secret), &self.secret.fallback());
        Arc::new(KeyMaterial::new(digest(secret), Source::Env, legacy))
    }

    /// 显式受管密钥（Node `resolveManagedSecret`，不发现缺省文件）。
    async fn explicit(&self) -> Result<Option<RedactedString>, CredentialError> {
        let secret = match (&self.secret.explicit, &self.secret.explicit_file) {
            (Some(_), Some(_)) => Err(
                "ZCODE_CREDENTIAL_SECRET and ZCODE_CREDENTIAL_SECRET_FILE are both set".to_owned(),
            ),
            (Some(value), None) => Ok(Some(value.expose().to_owned())),
            (None, Some(path)) => managed_file(path, Discovery::Explicit).await,
            (None, None) => Ok(None),
        };
        secret
            .map(|secret| secret.map(RedactedString::new))
            .map_err(refuse)
    }

    /// 系统路径：在 keyring 锁里解析。外层 `Err` 是没拿到锁（没碰凭据库），不缓存。
    async fn resolve_system(
        &self,
        create: bool,
    ) -> Result<Result<Arc<KeyMaterial>, CredentialError>, CredentialError> {
        let mut lock = self.file.clone().into_os_string();
        lock.push(KEYRING_LOCK_SUFFIX);
        let locked =
            rust_cli_fs::with_file_lock(Path::new(&lock), &LockOptions::default(), async || {
                Ok::<_, io::Error>(self.locked(create).await)
            })
            .await;
        match locked {
            Ok(outcome) => Ok(outcome.map_err(refuse)),
            // 另一个进程持锁等用户授权、超过等锁上限时，这里没有访问凭据库：不缓存这次失败，否则锁放开之后本进程仍然一直
            // 失败（Node 修过的同一个 bug，platform.md §13.5）。下一次调用重新等锁。
            Err(error) => Err(refuse(format!("the credential key lock: {error}"))),
        }
    }

    /// keyring 锁里的一步一步（platform.md §13.2 第 2 条）。
    async fn locked(&self, create: bool) -> Result<Arc<KeyMaterial>, Refusal> {
        let pinned = self.pinned().await?;
        let raw = read(&self.file)
            .await?
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        // 来源一旦是系统凭据库（固定过，或已有 v2:os 数据）就不再换
        let use_os = pinned || raw.contains(&Source::Os.marker());
        if !use_os {
            let default_file = self.file.with_file_name(SECRET_FILE);
            if let Some(secret) = managed_file(&default_file, Discovery::Default).await? {
                return Ok(self.env_key(&secret));
            }
            if raw.contains(&Source::Env.marker()) {
                return Err(
                    "the credentials need a managed secret that is not configured".to_owned(),
                );
            }
        }
        let mut encoded = self.read_store().await?;
        // 已有 v2 数据时绝不生成替代密钥（缺密钥是存储错误）
        if encoded.is_none() && create && !use_os && !raw.contains(V2_PREFIX) {
            let generated = RedactedString::new(
                BASE64URL.encode(random_bytes::<KEY_BYTES>().map_err(|error| error.to_string())?),
            );
            self.store
                .write(SERVICE, &self.account, &generated)
                .await
                .map_err(store_failure)?;
            // 写完再读一次：首次并发启动时只能有一个密钥
            if self.read_store().await?.as_ref() != Some(&generated) {
                return Err("the system store did not keep the new key".to_owned());
            }
            encoded = Some(generated);
        }
        let encoded = encoded.ok_or("the system store has no key for these credentials")?;
        let key =
            decode_key(encoded.expose()).ok_or("the stored key is not 32 bytes of base64url")?;
        if !pinned {
            rust_cli_fs::atomic_write(
                &self.file.with_file_name(SOURCE_FILE),
                PINNED_OS,
                PRIVATE_WRITE,
            )
            .await
            .map_err(|error| format!("{SOURCE_FILE}: {error}"))?;
        }
        let legacy = legacy_keys(None, &self.secret.fallback());
        Ok(Arc::new(KeyMaterial::new(key, Source::Os, legacy)))
    }

    async fn read_store(&self) -> Result<Option<RedactedString>, Refusal> {
        self.store
            .read(SERVICE, &self.account)
            .await
            .map_err(store_failure)
    }
}

fn store_failure(reason: String) -> Refusal {
    format!("the system credential store: {reason}")
}

/// 记下原因，交出不可用（文字照 Node，不带原因）。
fn refuse(reason: Refusal) -> CredentialError {
    tracing::warn!(reason = %reason, "the credential key is unavailable");
    CredentialError::Unavailable
}

async fn read(path: &Path) -> Result<Option<Vec<u8>>, Refusal> {
    rust_cli_fs::read_optional(path)
        .await
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// 43 个 base64url 字符、解出 32 字节。
fn decode_key(encoded: &str) -> Option<Key> {
    let valid = encoded.len() == ENCODED_KEY_CHARS
        && encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if !valid {
        return None;
    }
    BASE64URL.decode(encoded).ok()?.try_into().ok()
}

/// 受管密钥文件是怎么找到的：显式给的（`ZCODE_CREDENTIAL_SECRET_FILE`）不存在就是错误、不查权限位；缺省的
/// `credential-secret` 不存在就跳过，要只有本用户能读写。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Discovery {
    Explicit,
    Default,
}

/// 读受管密钥文件（Node `resolveManagedSecret`）：普通文件，按 UTF-8 解（坏字节换成替换符）后 32–4096 字节，
/// 内容原样（不去空白）。
async fn managed_file(path: &Path, discovery: Discovery) -> Result<Option<String>, Refusal> {
    let shown = path.display();
    let facts = match rust_cli_fs::file_facts(path).await {
        Ok(Some(facts)) => facts,
        Ok(None) if discovery == Discovery::Default => return Ok(None),
        Ok(None) => return Err(format!("{shown} does not exist")),
        Err(error) => return Err(format!("{shown}: {error}")),
    };
    if !facts.is_file {
        return Err(format!("{shown} is not a regular file"));
    }
    if discovery == Discovery::Default
        && facts.mode.is_some_and(|mode| mode & GROUP_OTHER_BITS != 0)
    {
        return Err(format!("{shown} is readable or writable by other users"));
    }
    let bytes = read(path)
        .await?
        .ok_or_else(|| format!("{shown} does not exist"))?;
    let secret = String::from_utf8_lossy(&bytes).into_owned();
    if !MANAGED_SECRET_BYTES.contains(&secret.len()) {
        let (least, most) = (MANAGED_SECRET_BYTES.start(), MANAGED_SECRET_BYTES.end());
        return Err(format!("{shown} must hold {least}–{most} bytes"));
    }
    Ok(Some(secret))
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
