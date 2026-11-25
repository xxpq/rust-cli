// 复制自 apps/zcode-host/crates/account/src/account/credential.rs（1f0bfd1b71）的 `CredentialCipher` 与读取
// 部分。改动：文件经 rust-cli-fs 读写；密钥构造失败返回错误而不是 panic；密钥材料（平台、home、用户名）
// 由入口给出，不在这里读进程环境。写入取 Node `shared-credentials.ts` 的 `mutateRawCredentialRecord`
// （33a9541140）：目录锁内读改写，原子写回（platform.md §5.5）。CRED-V2 起格式、密钥来源与迁移照 platform.md §13。
//! 共享凭据 `credentials.json`（docs/specs/rust-cli/platform.md §5.1、§13）。
//!
//! 与 Node Host、Node CLI 共用。新文件与已有 v2 的文件写 `enc:v2:<来源>:<指纹>:<iv>.<tag>.<ciphertext>`（AES-256-GCM，
//! 无填充 base64url），密钥是系统凭据库里的随机密钥或受管密钥（[`keys`]）；只有 `enc:v1:` 与明文的文件照原版本读写，
//! 不迁移（用户决定 2026-11-11，迁移留给 App 与 Node CLI）。不缓存文件内容：每次读取都以磁盘为准；只缓存 v2 的密钥。
//! 写入在与 Node 同形的目录锁内读改写（[`CredentialStore::mutate`]）。
//!
//! 委托模式（platform.md §13.13，接 App 的入口）：同一个文件、同一把锁、同一种原子写，只是值的加解密经 kernel-api 的
//! [`CredentialCipher`] 交给别的进程（App 里是 Main，经 Host），本进程不取密钥、不碰系统凭据库。

mod format;
mod keys;
mod seal;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::join_all;
use rust_cli_base::secret::RedactedString;
use rust_cli_fs::{FileMode, LockOptions, ParentDir, TempName, WriteOptions};
use rust_cli_kernel_api::credential_cipher::{CipherFailure, CredentialCipher};
use rust_cli_paths::StoragePath;
use serde_json::{Map, Value};

use format::{FileKey, is_v2};
use keys::KeySource;
pub use keys::SystemKeyStore;
/// 加密强度的随机字节（README §4 的随机类别）：MCP OAuth 的 PKCE、state 与 generation 也取这里（ext.md §3.12.6）。
pub(crate) use seal::random_bytes;

/// Node `atomicWritePrivateTextFile`：隐藏临时文件、0600、rename 被占用时的重试间隔。
pub(crate) const PRIVATE_WRITE: WriteOptions = WriteOptions {
    mode: FileMode::Private,
    temp_name: TempName::Hidden,
    parent: ParentDir::Create,
    rename_retry: rust_cli_fs::NODE_RENAME_RETRY,
};

/// 凭据库不可用的文字（Node `unavailable()` 的原文，platform.md §13.6）。
const UNAVAILABLE: &str = "System credential storage is unavailable. Unlock the OS credential store and restart to retry; headless servers require Secret Service, ZCODE_CREDENTIAL_SECRET, or a readable ZCODE_CREDENTIAL_SECRET_FILE of 32–4096 bytes. Do not configure both managed secret sources.";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// 解密失败（`凭据解密失败：<原因>`）。
    #[error("凭据解密失败：{0}")]
    Decrypt(&'static str),
    /// 系统凭据库不可用、缺密钥、来源或指纹不符、受管密钥配置有误：存储错误，不是登录失效（platform.md §13.6）。
    #[error("{}", UNAVAILABLE)]
    Unavailable,
    #[error("ZCode credentials are corrupt: {0}")]
    Corrupt(String),
    #[error("unable to read ZCode credentials: {0}")]
    Io(String),
    #[error("cannot encrypt the credential: {0}")]
    Encrypt(&'static str),
    #[error("unable to write ZCode credentials: {0}")]
    Write(String),
    /// 委托模式里加解密方不可用（platform.md §13.13）：存储错误，不是登录失效，不删凭据。
    #[error("the credential cipher is unavailable: {0}")]
    Delegated(String),
}

/// 委托模式里读没有声明要读的键：调用方的错（读要先经 [`CredentialStore::read_keys`] 声明）。
const UNDECLARED_READ: &str = "the credential was not declared for reading";
/// 委托模式里加密报了解不开（加密不会解值）。
const ENCRYPT_REFUSED: &str = "the delegated cipher refused to encrypt";

impl From<CipherFailure> for CredentialError {
    fn from(failure: CipherFailure) -> Self {
        match failure {
            CipherFailure::Decrypt => Self::Decrypt("Invalid encrypted credential"),
            CipherFailure::Unavailable(message) => Self::Delegated(message),
        }
    }
}

/// 凭据密钥的材料（入口在启动时读好传入，platform.md §13.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSecret {
    /// `ZCODE_CREDENTIAL_SECRET` 的原值（不去空白；空串当没设）。
    pub explicit: Option<RedactedString>,
    /// `ZCODE_CREDENTIAL_SECRET_FILE`（空串当没设；相对路径相对进程的 cwd，与 Node 相同）。
    pub explicit_file: Option<PathBuf>,
    /// Node `os.platform()`：darwin / linux / win32。
    pub platform: String,
    /// Node `os.homedir()`。
    pub home: String,
    /// Node `os.userInfo().username`。
    pub username: String,
}

impl CredentialSecret {
    /// v1 的缺省派生（Node `legacyKeyCandidates` 的 fallback）。
    fn fallback(&self) -> String {
        format!(
            "zcode-credential-fallback:{}:{}:{}",
            self.platform, self.home, self.username
        )
    }
}

/// 这一次读到的文件的密钥（按文件的版本，platform.md §13.1）。
#[derive(Clone)]
enum Keyed {
    /// 新文件（没有值）：读不要密钥，写入时再取 v2 的（允许新建）。
    New,
    Ready(FileKey),
    /// 读改写里取不到：解值时按值报出，只删除的改动照常提交（照 Node 的纯删除，platform.md §13.4）。
    Failed(CredentialError),
    /// 委托模式：声明要读的键已经经端口解好（每个键一个结果，platform.md §13.13）；别的键不能读。
    Opened(HashMap<String, Result<RedactedString, CredentialError>>),
}

/// 一次读取的凭据记录：值按需解密；改动里写入的值在 [`CredentialStore::mutate`] 结束时统一加密。
pub(crate) struct Credentials {
    record: Map<String, Value>,
    key: Keyed,
    /// 这次改动写入的明文。键已经按 Node 的顺序放进 `record`（已有的键原位，新键在末尾），值在加密后换上。
    writes: HashMap<String, RedactedString>,
}

impl Credentials {
    fn new(record: Map<String, Value>, key: Keyed) -> Self {
        Self {
            record,
            key,
            writes: HashMap::new(),
        }
    }

    /// 一个键的明文；没有这个键时为空。
    pub fn get(&self, key: &str) -> Result<Option<RedactedString>, CredentialError> {
        if let Some(plain) = self.writes.get(key) {
            return Ok(Some(plain.clone()));
        }
        let Some(raw) = self.record.get(key).and_then(Value::as_str) else {
            return Ok(None);
        };
        match &self.key {
            Keyed::Ready(file_key) => Ok(Some(RedactedString::new(file_key.decrypt(raw)?))),
            Keyed::Failed(error) => Err(error.clone()),
            // 新文件没有已存的值：能走到这里的只有本次改动写入的键，上面已经交出
            Keyed::New => Ok(None),
            Keyed::Opened(opened) => match opened.get(key) {
                Some(plain) => plain.clone().map(Some),
                None => Err(CredentialError::Delegated(UNDECLARED_READ.to_owned())),
            },
        }
    }

    /// 一个键存下的值原样（不解密）：比较「还是不是读到的那一份」用（MCP OAuth 的失效，ext.md §3.12.4）。
    pub fn stored(&self, key: &str) -> Option<&str> {
        if self.writes.contains_key(key) {
            return None;
        }
        self.record.get(key).and_then(Value::as_str)
    }

    /// 写入一个键（Node `save`）：值在这次改动结束时按文件的版本加密。
    pub fn set(&mut self, key: &str, value: &str) {
        self.record.insert(key.to_owned(), Value::Null);
        self.writes
            .insert(key.to_owned(), RedactedString::new(value));
    }

    /// 删掉一个键（不需要解密）。
    pub fn remove(&mut self, key: &str) {
        self.record.shift_remove(key);
        self.writes.remove(key);
    }

    /// 明文去掉首尾空白后非空。解不开时按没有处理（Node：坏密文不能当作已登录）。
    pub fn present(&self, key: &str) -> Option<RedactedString> {
        match self.get(key) {
            Ok(Some(value)) if !value.expose().trim().is_empty() => {
                Some(RedactedString::new(value.expose().trim()))
            }
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, "ignoring an undecryptable credential");
                None
            }
        }
    }

    /// 把写入的明文加密：v2 文件与 v1 文件用这次读到的密钥；新文件取 v2 的密钥（允许新建）。取不到密钥时报错，
    /// 什么都不写。委托模式每个值经端口加密。
    async fn seal(&mut self, sealer: &Sealer) -> Result<(), CredentialError> {
        if self.writes.is_empty() {
            return Ok(());
        }
        let keys = match sealer {
            Sealer::Local(keys) => keys,
            Sealer::Delegated(cipher) => {
                for (key, plain) in self.writes.drain() {
                    let sealed = cipher
                        .encrypt(&plain)
                        .await
                        .map_err(|failure| match failure {
                            CipherFailure::Decrypt => CredentialError::Encrypt(ENCRYPT_REFUSED),
                            CipherFailure::Unavailable(message) => {
                                CredentialError::Delegated(message)
                            }
                        })?;
                    self.record.insert(key, Value::String(sealed));
                }
                return Ok(());
            }
        };
        let file_key = match &self.key {
            Keyed::Ready(file_key) => file_key.clone(),
            Keyed::New => FileKey::V2(keys.key(true).await?),
            Keyed::Failed(error) => return Err(error.clone()),
            // 委托模式的值只在上面那一支加密
            Keyed::Opened(_) => return Err(CredentialError::Encrypt(ENCRYPT_REFUSED)),
        };
        for (key, plain) in self.writes.drain() {
            let sealed = file_key.encrypt(plain.expose())?;
            self.record.insert(key, Value::String(sealed));
        }
        Ok(())
    }
}

/// 值由谁加解密（platform.md §13.13）。
#[derive(Clone)]
enum Sealer {
    /// 本进程按文件的版本取密钥（§13.2、§13.4）。
    Local(Arc<KeySource>),
    /// 每个值经端口交给别的进程（App 里是 Main，经 Host）。
    Delegated(Arc<dyn CredentialCipher>),
}

/// `credentials.json` 的读写方。
#[derive(Clone)]
pub(crate) struct CredentialStore {
    file: StoragePath,
    sealer: Sealer,
}

impl CredentialStore {
    pub fn new(
        file: StoragePath,
        secret: &CredentialSecret,
        store: Arc<dyn SystemKeyStore>,
    ) -> Self {
        let keys = Arc::new(KeySource::new(file.as_path(), secret, store));
        Self {
            file,
            sealer: Sealer::Local(keys),
        }
    }

    /// 委托模式（platform.md §13.13）：同一个文件，值经 `cipher` 加解密。
    pub fn delegated(file: StoragePath, cipher: Arc<dyn CredentialCipher>) -> Self {
        Self {
            file,
            sealer: Sealer::Delegated(cipher),
        }
    }

    pub fn file(&self) -> &StoragePath {
        &self.file
    }

    /// 读整个文件，按文件的版本取密钥（不迁移）。取不到密钥时整次读失败（[`CredentialError::Unavailable`]）。
    pub async fn read(&self) -> Result<Credentials, CredentialError> {
        let record = self.record().await?;
        match self.keyed(&record, &[]).await {
            Keyed::Failed(error) => Err(error),
            key => Ok(Credentials::new(record, key)),
        }
    }

    /// 只要这几个键（MCP OAuth 的每个请求都读，docs/specs/rust-cli/ext.md §3.12.4）：一个都不在时不取密钥、不碰系统凭据库，
    /// 返回 None；在时同 [`Self::read`]。委托模式只解这几个键（每个在文件里的键经端口一次）。
    pub async fn read_keys(&self, keys: &[String]) -> Result<Option<Credentials>, CredentialError> {
        let record = self.record().await?;
        if !keys.iter().any(|key| record.contains_key(key)) {
            return Ok(None);
        }
        match self.keyed(&record, keys).await {
            Keyed::Failed(error) => Err(error),
            key => Ok(Some(Credentials::new(record, key))),
        }
    }

    /// 这几个键里有没有在文件里的（不解密，不取密钥）。
    pub async fn contains_any(&self, keys: &[String]) -> Result<bool, CredentialError> {
        let record = self.record().await?;
        Ok(keys.iter().any(|key| record.contains_key(key)))
    }

    /// 在目录锁内读改写（Node `mutateRawCredentialRecord`）：持锁读文件（版本按这一次读到的为准，platform.md §13.5），
    /// `change` 改记录，写入的值按文件的版本加密，原子写回 `JSON.stringify(record, null, 2)` 加换行。取不到密钥时只删除的
    /// 改动照常提交（照 Node 的纯删除）。文件损坏时不写（不能把别的进程的凭据全部抹掉）；`change` 失败时也不写。
    pub async fn mutate<T>(
        &self,
        change: impl FnOnce(&mut Credentials) -> Result<T, CredentialError>,
    ) -> Result<T, CredentialError> {
        let file = self.file.as_path();
        rust_cli_fs::with_file_lock(file, &LockOptions::default(), async || {
            let record = self.record().await?;
            let key = self.keyed(&record, &[]).await;
            let mut credentials = Credentials::new(record, key);
            let result = change(&mut credentials)?;
            credentials.seal(&self.sealer).await?;
            self.write(&credentials.record).await?;
            Ok(result)
        })
        .await
    }

    /// 按当前文件的版本先取一次写入用的密钥（Node `assertWritable`，platform.md §13.7）：OAuth 登录在发起授权之前确认能写。
    pub async fn writable(&self) -> Result<(), CredentialError> {
        let record = self.record().await?;
        let Sealer::Local(keys) = &self.sealer else {
            // 委托模式没有本进程的密钥要先取（登录不走这里，platform.md §13.13）
            return Ok(());
        };
        match self.keyed(&record, &[]).await {
            Keyed::New => keys.key(true).await.map(drop),
            Keyed::Ready(_) | Keyed::Opened(_) => Ok(()),
            Keyed::Failed(error) => Err(error),
        }
    }

    /// 文件的版本与它的密钥：有 v2 值是 v2 文件（取 v2 的密钥，不新建）；有值而没有 v2 是 v1 文件（不碰系统凭据库）；
    /// 没有值是新文件。委托模式不看版本：`reading` 里在文件里的键经端口解好（并发）。
    async fn keyed(&self, record: &Map<String, Value>, reading: &[String]) -> Keyed {
        let keys = match &self.sealer {
            Sealer::Local(keys) => keys,
            Sealer::Delegated(cipher) => {
                let present: Vec<(&String, &str)> = reading
                    .iter()
                    .filter_map(|key| Some((key, record.get(key)?.as_str()?)))
                    .collect();
                let opened =
                    join_all(present.iter().map(|(_, sealed)| cipher.decrypt(sealed))).await;
                return Keyed::Opened(
                    present
                        .into_iter()
                        .zip(opened)
                        .map(|((key, _), plain)| {
                            (key.clone(), plain.map_err(CredentialError::from))
                        })
                        .collect(),
                );
            }
        };
        if record.is_empty() {
            return Keyed::New;
        }
        let key = if record.values().filter_map(Value::as_str).any(is_v2) {
            keys.key(false).await.map(FileKey::V2)
        } else {
            keys.legacy(record).await.map(FileKey::V1)
        };
        match key {
            Ok(key) => Keyed::Ready(key),
            Err(error) => Keyed::Failed(error),
        }
    }

    /// 读整个文件。不存在时是空记录；不是“值全为字符串的对象”时报损坏（Node 拒绝把它当空 store）。
    async fn record(&self) -> Result<Map<String, Value>, CredentialError> {
        let bytes = rust_cli_fs::read_optional(self.file.as_path())
            .await
            .map_err(|error| {
                CredentialError::Io(format!("{}: {error}", self.file.as_path().display()))
            })?;
        match bytes {
            None => Ok(Map::new()),
            Some(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Object(record)) if record.values().all(Value::is_string) => Ok(record),
                _ => Err(CredentialError::Corrupt(
                    self.file.as_path().display().to_string(),
                )),
            },
        }
    }

    async fn write(&self, record: &Map<String, Value>) -> Result<(), CredentialError> {
        let file = self.file.as_path();
        let mut text = serde_json::to_string_pretty(record)
            .map_err(|error| CredentialError::Write(error.to_string()))?;
        text.push('\n');
        rust_cli_fs::atomic_write(file, text.as_bytes(), PRIVATE_WRITE)
            .await
            .map_err(|error| CredentialError::Write(format!("{}: {error}", file.display())))
    }
}

impl From<std::io::Error> for CredentialError {
    /// 目录锁的错误（超时、权限）。
    fn from(error: std::io::Error) -> Self {
        Self::Write(error.to_string())
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "delegated_tests.rs"]
mod delegated_tests;
