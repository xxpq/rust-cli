//! 令牌存放的键与锁（docs/specs/rust-cli/ext.md §3.12.4）：与 Node 逐字相同（`oauth.ts` 的 `createCredentialKeyPrefix`、
//! `oauth-credentials.ts` 与 `oauth-shared.ts` 的键名、`oauth-refresh.ts` 的 `resolveRefreshLockPath`），两边读写同一组键、
//! 抢同一把刷新锁。
use std::path::{Path, PathBuf};

use rust_cli_kernel_api::mcp_auth::McpAuthTarget;

/// 键的前缀：`mcp:oauth:<hash>`。
const PREFIX: &str = "mcp:oauth:";
/// 哈希取多少个十六进制字符。
const HASH_CHARS: usize = 24;
/// canonical：client 与令牌成对发布。
pub(super) const CANONICAL: &str = "authorization_credentials";
/// legacy 镜像（Node 的兼容窗口）。
pub(super) const LEGACY_CLIENT: &str = "client_information";
pub(super) const LEGACY_TOKENS: &str = "tokens";
/// 发现的结果与取得的时刻。
pub(super) const DISCOVERY: &str = "discovery_state";
pub(super) const DISCOVERY_FETCHED_AT: &str = "discovery_state_fetched_at";
/// 刷新锁文件的后缀。
const REFRESH_LOCK_SUFFIX: &str = ".refresh";

/// 一个服务器的一组键。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Keys {
    prefix: String,
}

impl Keys {
    /// Node `createCredentialKeyPrefix`：sha256(`名字\nURL\nclientId\nscope\nredirectPath`)，没写的字段是空串。
    pub(super) fn of(target: &McpAuthTarget) -> Self {
        let oauth = &target.oauth;
        let material = [
            target.server.as_str(),
            target.url.as_str(),
            oauth.client_id.as_deref().unwrap_or_default(),
            oauth.scope.as_deref().unwrap_or_default(),
            oauth.redirect_path.as_deref().unwrap_or_default(),
        ]
        .join("\n");
        let hex = rust_cli_base::hash::sha256_hex(material.as_bytes());
        Self {
            prefix: format!("{PREFIX}{}", &hex[..HASH_CHARS]),
        }
    }

    pub(super) fn prefix(&self) -> &str {
        &self.prefix
    }

    pub(super) fn key(&self, name: &str) -> String {
        format!("{}:{name}", self.prefix)
    }

    /// 令牌的三个键（canonical 与两份镜像）。
    pub(super) fn pair(&self) -> [String; 3] {
        [
            self.key(CANONICAL),
            self.key(LEGACY_CLIENT),
            self.key(LEGACY_TOKENS),
        ]
    }

    /// 刷新锁：凭据文件同目录的 `mcp-oauth-<hash>.refresh`（前缀里不是 `[a-zA-Z0-9-]` 的字符换成 `-`，Windows 的文件名里不能有冒号）。
    pub(super) fn refresh_lock(&self, credentials: &Path) -> PathBuf {
        let name: String = self
            .prefix
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        credentials
            .parent()
            .unwrap_or(Path::new(""))
            .join(format!("{name}{REFRESH_LOCK_SUFFIX}"))
    }
}
