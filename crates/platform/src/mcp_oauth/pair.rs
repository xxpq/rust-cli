//! 令牌的记录（docs/specs/rust-cli/ext.md §3.12.4）：Node 的 canonical pair 与 legacy 镜像怎样读、怎样写。读的规则照 Node
//! `oauth-credentials.ts` 的 `deriveCredentialPair`（纯函数，一份实现）；写出的形状同 `publishCanonicalCredentials`。
use rust_cli_base::secret::RedactedString;
use rust_cli_base::units;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

/// canonical 现在的版本；读认 1 与 2（Node `MCP_OAUTH_SUPPORTED_CREDENTIAL_VERSIONS`）。
pub(super) const VERSION: u8 = 2;
const LEGACY_VERSION: u8 = 1;
/// 没有 `generation` 的旧记录：用原文的 sha256 代替（Node `resolveCredentialGeneration`）。
const LEGACY_GENERATION: &str = "legacy-";
const LEGACY_GENERATION_CHARS: usize = 32;

/// 令牌（TS SDK `OAuthTokens`）。秘密值是 [`RedactedString`]（序列化成原文，Debug 不含原值）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Tokens {
    pub access_token: RedactedString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<RedactedString>,
    pub token_type: String,
    /// 秒；数字原样（整数还是小数）保存，读写都不改写法。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<RedactedString>,
}

/// canonical 的形状（Node `McpOAuthCanonicalCredentials`，键按字母序，同 Node 写出的顺序）。时刻是 Unix 毫秒。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Canonical {
    pub client_information: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obtained_at: Option<Number>,
    pub published_by: String,
    pub tokens: Tokens,
    pub version: u8,
}

impl Canonical {
    /// Node `isCanonicalCredentials`：版本认识、`published_by` 非空、client 有字符串的 `client_id`、令牌有字符串的
    /// `access_token` 与 `token_type`。不合格的当作没有 canonical（不删：可能是更新的版本写的）。
    fn parse(raw: &str) -> Option<Self> {
        let canonical: Self = serde_json::from_str(raw).ok()?;
        let known = matches!(canonical.version, LEGACY_VERSION | VERSION);
        let client = canonical
            .client_information
            .get("client_id")
            .is_some_and(Value::is_string);
        (known && !canonical.published_by.is_empty() && client).then_some(canonical)
    }

    /// 每次发布唯一的 ID；旧记录没有时用原文的哈希。
    fn generation_of(&self, raw: &str) -> String {
        match self.generation.as_deref().filter(|g| !g.is_empty()) {
            Some(generation) => generation.to_owned(),
            None => {
                let hex = rust_cli_base::hash::sha256_hex(raw.as_bytes());
                format!("{LEGACY_GENERATION}{}", &hex[..LEGACY_GENERATION_CHARS])
            }
        }
    }
}

/// 派生出的一份可用的 client 与令牌（Node `CredentialPairSnapshot`）。
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct Pair {
    pub client: Option<Map<String, Value>>,
    pub tokens: Option<Tokens>,
    pub expires_at: Option<f64>,
    pub issuer: Option<String>,
    /// canonical 的原文；只有 legacy 时为 None。
    pub raw: Option<String>,
    pub generation: Option<String>,
    /// canonical 存下的值原样（密文，失效时比它、相同才删，ext.md §3.12.4）：读文件的一方填（[`derive`] 只看明文）。
    pub sealed: Option<String>,
}

/// 读到的三个键（明文）。
#[derive(Debug, Default)]
pub(super) struct Stored {
    pub canonical: Option<String>,
    pub legacy_client: Option<String>,
    pub legacy_tokens: Option<String>,
}

fn json<T: for<'de> Deserialize<'de>>(raw: Option<&str>) -> Option<T> {
    serde_json::from_str(raw?).ok()
}

/// Node `deriveCredentialPair`：兼容窗口里 CLI 与桌面端可能是不同版本，旧进程只写 legacy，新进程写 canonical 与镜像；这里判断
/// legacy 的变化能不能证明属于同一次授权。证明不了时不拼接两个事务的 client 与令牌。
pub(super) fn derive(stored: &Stored) -> Option<Pair> {
    let legacy_client: Option<Map<String, Value>> = json(stored.legacy_client.as_deref());
    let legacy_tokens: Option<Tokens> = json(stored.legacy_tokens.as_deref());
    let canonical = stored
        .canonical
        .as_deref()
        .and_then(|raw| Canonical::parse(raw).map(|canonical| (raw, canonical)));
    let Some((raw, canonical)) = canonical else {
        if legacy_client.is_none() && legacy_tokens.is_none() {
            return None;
        }
        return Some(Pair {
            client: legacy_client,
            tokens: legacy_tokens,
            ..Pair::default()
        });
    };
    let snapshot = Pair {
        client: Some(canonical.client_information.clone()),
        tokens: Some(canonical.tokens.clone()),
        expires_at: canonical.expires_at.as_ref().and_then(Number::as_f64),
        issuer: canonical.issuer.clone(),
        raw: Some(raw.to_owned()),
        generation: Some(canonical.generation_of(raw)),
        sealed: None,
    };
    let legacy = |client: Option<Map<String, Value>>, tokens: Option<Tokens>| Pair {
        client,
        tokens,
        ..Pair::default()
    };
    if canonical.version == LEGACY_VERSION {
        // v1 发布之后删掉镜像，没有镜像是稳态；旧进程重新写了 legacy 令牌、client 没变时采用它。
        let same_client = legacy_client
            .as_ref()
            .is_none_or(|client| *client == canonical.client_information);
        return match legacy_tokens {
            Some(tokens) if same_client => Some(legacy(
                Some(legacy_client.unwrap_or(canonical.client_information)),
                Some(tokens),
            )),
            _ => Some(snapshot),
        };
    }
    let Some(tokens) = legacy_tokens else {
        // 旧进程作废了令牌：保持作废，不从 canonical 复活。
        return Some(legacy(legacy_client, None));
    };
    if tokens == canonical.tokens {
        return Some(match legacy_client {
            Some(_) => snapshot,
            None => legacy(None, Some(tokens)),
        });
    }
    if legacy_client.as_ref() == Some(&canonical.client_information) {
        // client 没变：legacy 的令牌是同一个身份的旧进程刷新的结果。
        return Some(legacy(legacy_client, Some(tokens)));
    }
    // client 与令牌都变了、又没有 generation：证明不了来自同一次授权，留下 client、要再授权。
    Some(legacy(legacy_client, None))
}

/// 要发布的三个键的明文（Node `publishCanonicalCredentials`）：canonical 与两份镜像。
pub(super) struct Published {
    pub canonical: String,
    pub legacy_client: String,
    pub legacy_tokens: String,
}

pub(super) struct Publish<'a> {
    pub client: &'a Map<String, Value>,
    pub tokens: &'a Tokens,
    pub issuer: Option<&'a str>,
    pub published_by: &'a str,
    pub obtained_at_ms: u64,
    pub generation: String,
}

/// 过期的时刻 = 取得的时刻 + `expires_in` 秒（Node `publishCanonicalCredentials`）。秒是整数时结果也是整数。
#[expect(
    clippy::cast_precision_loss,
    reason = "Unix 毫秒在 2^53 以内，与 Node 的 number 相同"
)]
fn expires_at(obtained_ms: u64, expires_in: Option<&Number>) -> Option<Number> {
    let seconds = expires_in?;
    match seconds.as_u64() {
        Some(whole) => Some(Number::from(
            obtained_ms.saturating_add(whole.saturating_mul(units::MS_PER_SECOND)),
        )),
        None => Number::from_f64(obtained_ms as f64 + seconds.as_f64()? * units::MS_PER_SECOND_F64),
    }
}

pub(super) fn publish(input: Publish<'_>) -> serde_json::Result<Published> {
    let record = Canonical {
        client_information: input.client.clone(),
        expires_at: expires_at(input.obtained_at_ms, input.tokens.expires_in.as_ref()),
        generation: Some(input.generation),
        issuer: input.issuer.map(str::to_owned),
        obtained_at: Some(Number::from(input.obtained_at_ms)),
        published_by: input.published_by.to_owned(),
        tokens: input.tokens.clone(),
        version: VERSION,
    };
    Ok(Published {
        canonical: serde_json::to_string(&record)?,
        legacy_client: serde_json::to_string(input.client)?,
        legacy_tokens: serde_json::to_string(input.tokens)?,
    })
}

#[cfg(test)]
#[path = "pair_tests.rs"]
mod tests;
