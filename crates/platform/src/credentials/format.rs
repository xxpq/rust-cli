//! 值的三种格式与一个凭据文件的密钥（docs/specs/rust-cli/platform.md §13.1、§13.2、§13.4）：没有 `enc:` 前缀的旧明文、
//! `enc:v1:<段>`、`enc:v2:<来源>:<指纹>:<段>`。v2 文件与新文件用 [`KeyMaterial`]，只有 v1 与明文的文件用
//! [`LegacyKeys`]（rust 不迁移，用户决定 2026-11-11）。

use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::CredentialError;
use super::seal::{Key, open, seal};

/// 旧格式（v1 文件照它读写）。
pub(crate) const V1_PREFIX: &str = "enc:v1:";
/// 现在的格式（新文件与 v2 文件写它）。
pub(crate) const V2_PREFIX: &str = "enc:v2:";
/// 加密值的共同前缀：别的版本号是解密失败，不当明文。
const ENCRYPTED_PREFIX: &str = "enc:";
/// v2 的指纹：sha256(密钥) 的 hex 前 24 个字符。
const FINGERPRINT_HEX_CHARS: usize = 24;

/// v2 密钥的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// 系统凭据库里的随机密钥。
    Os,
    /// 受管密钥（`ZCODE_CREDENTIAL_SECRET`、`ZCODE_CREDENTIAL_SECRET_FILE`、凭据目录的 `credential-secret`）。
    Env,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Self::Os => "os",
            Self::Env => "env",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Os, Self::Env]
            .into_iter()
            .find(|source| source.as_str() == text)
    }

    /// 这个来源的 v2 值的开头（`enc:v2:os:`）：密钥解析时按它判断文件里已有哪种来源的数据。
    pub(crate) fn marker(self) -> String {
        format!("{V2_PREFIX}{}:", self.as_str())
    }
}

/// 文件里有 v2 的值：这个文件是 v2 文件（platform.md §13.1）。
pub(crate) fn is_v2(value: &str) -> bool {
    value.starts_with(V2_PREFIX)
}

/// sha256 当作 AES 密钥（受管密钥与 v1 的候选密钥，Node `digest`）。
pub(crate) fn digest(secret: &str) -> Key {
    Sha256::digest(secret.as_bytes()).into()
}

fn fingerprint(key: &Key) -> String {
    let mut hex = rust_cli_base::hash::sha256_hex(key);
    hex.truncate(FINGERPRINT_HEX_CHARS);
    hex
}

fn is_fingerprint(text: &str) -> bool {
    text.len() == FINGERPRINT_HEX_CHARS
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// v1 的候选密钥（Node `legacyKeyCandidates`，去重、按序试）：[secret 原值, secret 去空白, 缺省派生]，空的退到缺省派生。
/// 来源是系统凭据库时没有 secret，只有缺省派生。
pub(crate) fn legacy_keys(secret: Option<&str>, fallback: &str) -> Vec<Key> {
    let or_fallback = |value: Option<&str>| match value {
        Some(value) if !value.is_empty() => value.to_owned(),
        _ => fallback.to_owned(),
    };
    let candidates = [
        or_fallback(secret),
        or_fallback(secret.map(str::trim)),
        fallback.to_owned(),
    ];
    let mut keys: Vec<Key> = Vec::new();
    for key in candidates.iter().map(|candidate| digest(candidate)) {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// 一个凭据文件的密钥：v2 的密钥、来源与指纹，v1 的候选密钥。
#[derive(Clone)]
pub(crate) struct KeyMaterial {
    key: Key,
    source: Source,
    fingerprint: String,
    legacy: Vec<Key>,
}

impl KeyMaterial {
    pub(crate) fn new(key: Key, source: Source, legacy: Vec<Key>) -> Self {
        Self {
            fingerprint: fingerprint(&key),
            key,
            source,
            legacy,
        }
    }

    /// 加密成 v2（Node `encrypt`）。
    pub(crate) fn encrypt(&self, value: &str) -> Result<String, CredentialError> {
        Ok(format!(
            "{}{}:{}",
            self.source.marker(),
            self.fingerprint,
            seal(value, &self.key)?
        ))
    }

    /// 解一个值（Node `decrypt`）：v2 先核对来源与指纹；v2 文件里留着的 v1 与明文照读（不迁移）。
    pub(crate) fn decrypt(&self, value: &str) -> Result<String, CredentialError> {
        match value.strip_prefix(V2_PREFIX) {
            Some(rest) => open(self.matching(rest)?, &self.key),
            None => decrypt_legacy(value, &self.legacy),
        }
    }

    /// 拆 `enc:v2:` 之后用 `:` 分开的三段（来源、指纹、密文段），来源与指纹必须是这把密钥的；返回密文段。
    fn matching<'a>(&self, rest: &'a str) -> Result<&'a str, CredentialError> {
        let malformed = CredentialError::Decrypt("密文格式非法");
        let fields: Vec<&str> = rest.split(':').collect();
        let [source, print, payload] = fields.as_slice() else {
            return Err(malformed);
        };
        let Some(source) = Source::parse(source) else {
            return Err(malformed);
        };
        if !is_fingerprint(print) || payload.is_empty() {
            return Err(malformed);
        }
        // 来源或密钥换了（缺密钥、换了受管密钥）：存储错误，不是登录失效（platform.md §13.6）
        if source != self.source || *print != self.fingerprint {
            return Err(CredentialError::Unavailable);
        }
        Ok(payload)
    }
}

/// 明文原样；v1 依次试候选密钥，以 GCM 通过为准；别的 `enc:` 前缀是解密失败。
fn decrypt_legacy(value: &str, candidates: &[Key]) -> Result<String, CredentialError> {
    let Some(payload) = value.strip_prefix(V1_PREFIX) else {
        if value.starts_with(ENCRYPTED_PREFIX) {
            return Err(CredentialError::Decrypt("未知的密文版本"));
        }
        return Ok(value.to_owned());
    };
    let mut last = CredentialError::Decrypt("密钥不匹配或密文已损坏");
    for key in candidates {
        match open(payload, key) {
            Ok(plain) => return Ok(plain),
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// v1 文件的密钥（platform.md §13.2 第 4 条）：解用的候选密钥与写入用的那一个，不经系统凭据库。
#[derive(Clone)]
pub(crate) struct LegacyKeys {
    candidates: Vec<Key>,
    write: Key,
}

impl LegacyKeys {
    /// 写入用的密钥（Node `createLegacyServerCredentialCipher`）：候选密钥里按顺序第一个能解开某个已有 v1 值的；都解不开
    /// 时用 `otherwise`。这样后来放的受管密钥文件不会抢先换掉旧密钥，写出的值旧版本的 App 与 CLI 照样读得出。
    pub(crate) fn new<'a>(
        candidates: Vec<Key>,
        otherwise: Key,
        values: impl Iterator<Item = &'a str>,
    ) -> Self {
        let sealed: Vec<&str> = values
            .filter_map(|value| value.strip_prefix(V1_PREFIX))
            .collect();
        let write = candidates
            .iter()
            .find(|key| sealed.iter().any(|payload| open(payload, key).is_ok()))
            .copied()
            .unwrap_or(otherwise);
        Self { candidates, write }
    }
}

/// 一个凭据文件的密钥，按文件的版本（platform.md §13.1）。
#[derive(Clone)]
pub(crate) enum FileKey {
    /// v2 文件与新文件：写 v2。
    V2(Arc<KeyMaterial>),
    /// 只有 v1 与明文的文件：照原版本写 v1。
    V1(LegacyKeys),
}

impl FileKey {
    pub(crate) fn encrypt(&self, value: &str) -> Result<String, CredentialError> {
        match self {
            Self::V2(material) => material.encrypt(value),
            Self::V1(legacy) => Ok(format!("{V1_PREFIX}{}", seal(value, &legacy.write)?)),
        }
    }

    pub(crate) fn decrypt(&self, value: &str) -> Result<String, CredentialError> {
        match self {
            Self::V2(material) => material.decrypt(value),
            Self::V1(legacy) => decrypt_legacy(value, &legacy.candidates),
        }
    }
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
