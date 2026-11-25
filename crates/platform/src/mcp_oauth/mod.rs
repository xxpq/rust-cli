//! 远程 MCP 服务器的 OAuth（docs/specs/rust-cli/ext.md §3.12）：kernel-api 的 [`McpTokens`] 与 [`McpAuthorizer`] 的本机实现。
//!
//! 令牌在 `credentials.json`（与 Node、App 共用，键与形状见 [`keys`]、[`pair`]）；每次取都读文件，进程里不缓存令牌。HTTP 全部
//! 经 net 的 [`Egress`]，本机回调经 `net::loopback`。
mod discovery;
mod http;
mod keys;
mod oauth;
mod pair;
mod sign_in;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;
use rust_cli_fs::LockOptions;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::mcp::McpError;
use rust_cli_kernel_api::mcp_auth::{McpAuthTarget, McpAuthorizer, McpPendingSignIn, McpTokens};
use rust_cli_net::Egress;

use crate::credentials::{CredentialError, CredentialStore};
use discovery::Discovery;
use keys::{DISCOVERY, DISCOVERY_FETCHED_AT, Keys};
use oauth::Rejection;
use pair::{Pair, Stored};

/// 离过期不到这么久就先刷新（ext.md §3.12.3）。
const EXPIRY_SKEW_MS: u64 = 300_000;
/// 发现的结果用多久（Node `MCP_OAUTH_DISCOVERY_TTL_MS`）。
const DISCOVERY_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// 等刷新锁最久多久（Node `REFRESH_LOCK_MAX_WAIT_MS`：盖住锁里的一次发现与一次换令牌）。
const REFRESH_LOCK_WAIT: Duration = Duration::from_secs(45);
/// 刷新发布的 `published_by` 的前缀（Node `refresh:<前缀>`）。
const REFRESHED_BY: &str = "refresh:";
/// 存下的 client 没有 secret 时，配置了 `clientId` 的 client（不是动态注册的）遇到 `invalid_client` 报配置错误（Node 的文字）。
const STATIC_CLIENT_REJECTED: &str = "OAuth client was rejected by the authorization server (invalid_client). The configured clientId is not usable; fix the MCP oauth configuration.";

/// 本机的 MCP OAuth（实现两个端口）。
#[derive(Clone)]
pub struct McpOAuthClient {
    credentials: CredentialStore,
    egress: Arc<Egress>,
    clock: Arc<dyn Clock>,
}

fn store_error(error: CredentialError) -> McpError {
    McpError::Io {
        message: error.to_string(),
    }
}

fn auth_required(message: impl Into<String>) -> McpError {
    McpError::AuthRequired {
        message: message.into(),
        scope: None,
    }
}

fn temporary(message: impl Into<String>) -> McpError {
    McpError::Connect {
        message: message.into(),
    }
}

impl McpOAuthClient {
    pub(crate) fn new(
        credentials: CredentialStore,
        egress: Arc<Egress>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            credentials,
            egress,
            clock,
        }
    }

    /// 读三个键、按 Node 的规则派生（[`pair::derive`]）。三个键都不在时不碰系统凭据库。
    async fn pair(&self, keys: &Keys) -> Result<Option<Pair>, McpError> {
        let names = keys.pair();
        let Some(read) = self
            .credentials
            .read_keys(&names)
            .await
            .map_err(store_error)?
        else {
            return Ok(None);
        };
        let get = |name: &String| -> Result<Option<String>, McpError> {
            Ok(read
                .get(name)
                .map_err(store_error)?
                .map(|value| value.expose().to_owned()))
        };
        let stored = Stored {
            canonical: get(&names[0])?,
            legacy_client: get(&names[1])?,
            legacy_tokens: get(&names[2])?,
        };
        Ok(pair::derive(&stored).map(|mut pair| {
            if pair.raw.is_some() {
                pair.sealed = read.stored(&names[0]).map(str::to_owned);
            }
            pair
        }))
    }

    /// 发现的结果：缓存还能用就用，否则现取并存下（Node `resolveAsMetadata`）。
    async fn discovery(
        &self,
        keys: &Keys,
        target: &McpAuthTarget,
        issuer: Option<&str>,
    ) -> Result<Discovery, String> {
        let names = [keys.key(DISCOVERY), keys.key(DISCOVERY_FETCHED_AT)];
        if let Some(read) = self
            .credentials
            .read_keys(&names)
            .await
            .map_err(|e| e.to_string())?
        {
            // 缓存解不开时当作没有缓存、现取（Node `loadDiscoveryRecord` 对坏记录同样返回 undefined）：它只是可重建的缓存。
            let value =
                |name: &String| read.get(name).ok().flatten().map(|v| v.expose().to_owned());
            let (state, at) = (value(&names[0]), value(&names[1]));
            if let Some(found) =
                discovery::cached(state.as_deref(), at.as_deref(), self.clock.now_ms(), issuer)
            {
                return Ok(found);
            }
        }
        let found = discovery::discover(
            &self.egress,
            &target.url,
            target.oauth.metadata_url.as_deref(),
        )
        .await?;
        let state = serde_json::to_string(&found).map_err(|error| error.to_string())?;
        let at = self.clock.now_ms().to_string();
        self.credentials
            .mutate(|credentials| {
                credentials.set(&names[0], &state);
                credentials.set(&names[1], &at);
                Ok(())
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(found)
    }

    /// 发布一份 client 与令牌（canonical 与两份镜像，一次读改写，ext.md §3.12.4）。
    async fn publish(&self, keys: &Keys, input: pair::Publish<'_>) -> Result<(), McpError> {
        let published = pair::publish(input).map_err(|error| temporary(error.to_string()))?;
        let [canonical, client, tokens] = keys.pair();
        self.credentials
            .mutate(|credentials| {
                credentials.set(&canonical, &published.canonical);
                credentials.set(&client, &published.legacy_client);
                credentials.set(&tokens, &published.legacy_tokens);
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    /// 刷新得到确定的拒绝：canonical 仍是读到的那一份时才删（Node `invalidateCanonicalCredentials`）。`all` 时连 client 一起删。
    /// 比的是存下的值原样（不解密）：委托模式的读改写里不解值（platform.md §13.13）；每次发布的 generation 都是新的，原样相同就是同一份。
    async fn invalidate(
        &self,
        keys: &Keys,
        expected: Option<&str>,
        all: bool,
    ) -> Result<(), McpError> {
        let Some(expected) = expected else {
            return Ok(());
        };
        let [canonical, client, tokens] = keys.pair();
        self.credentials
            .mutate(|credentials| {
                if credentials.stored(&canonical) != Some(expected) {
                    return Ok(());
                }
                credentials.remove(&canonical);
                credentials.remove(&tokens);
                if all {
                    credentials.remove(&client);
                }
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    /// 在刷新锁里刷新（§3.12.5）。锁里重读：令牌已经不是 `seen` 了（别的进程刷新过），直接用它。`reactive` 是 401 之后：网络失败
    /// 报临时错误；否则（快过期的主动刷新）照用旧的。
    async fn refresh(
        &self,
        target: &McpAuthTarget,
        seen: Option<&RedactedString>,
        reactive: bool,
    ) -> Result<RedactedString, McpError> {
        let keys = Keys::of(target);
        let lock = keys.refresh_lock(self.credentials.file().as_path());
        let options = LockOptions {
            max_wait: REFRESH_LOCK_WAIT,
            ..LockOptions::default()
        };
        let locked = rust_cli_fs::with_file_lock(&lock, &options, async || {
            Ok::<_, std::io::Error>(self.refresh_locked(target, &keys, seen, reactive).await)
        })
        .await;
        match locked {
            Ok(outcome) => outcome,
            // 等锁超时不代表刷新失败：赢家可能已经发布了。重读，换了就用新的。
            Err(error) => match self.pair(&keys).await?.and_then(|pair| pair.tokens) {
                Some(tokens) if Some(&tokens.access_token) != seen => Ok(tokens.access_token),
                _ => Err(temporary(format!("MCP OAuth refresh lock: {error}"))),
            },
        }
    }

    async fn refresh_locked(
        &self,
        target: &McpAuthTarget,
        keys: &Keys,
        seen: Option<&RedactedString>,
        reactive: bool,
    ) -> Result<RedactedString, McpError> {
        let Some(current) = self.pair(keys).await? else {
            return Err(auth_required(format!(
                "MCP server \"{}\" has no stored OAuth tokens",
                target.server
            )));
        };
        let Some(tokens) = current.tokens.clone() else {
            return Err(auth_required(format!(
                "MCP server \"{}\" has no stored OAuth tokens",
                target.server
            )));
        };
        if Some(&tokens.access_token) != seen {
            return Ok(tokens.access_token);
        }
        let (Some(refresh_token), Some(client)) =
            (tokens.refresh_token.clone(), current.client.clone())
        else {
            return Err(auth_required(format!(
                "MCP server \"{}\" has no OAuth refresh token",
                target.server
            )));
        };
        let soft = |message: String| {
            if reactive {
                Err(temporary(message))
            } else {
                Ok(tokens.access_token.clone())
            }
        };
        let found = match self
            .discovery(keys, target, current.issuer.as_deref())
            .await
        {
            Ok(found) => found,
            Err(message) => return soft(message),
        };
        let resource = match found.resource(&target.url) {
            Ok(resource) => resource,
            Err(message) => return soft(message),
        };
        let built = match oauth::client(&client, &found) {
            Ok(built) => built,
            Err(message) => return soft(message),
        };
        let secret = oauth2::RefreshToken::new(refresh_token.expose().to_owned());
        let mut request = built.exchange_refresh_token(&secret);
        if let Some(resource) = &resource {
            request = request.add_extra_param("resource", resource);
        }
        let http = http::EgressClient(&self.egress);
        let response = match request.request_async(&http).await {
            Ok(response) => response,
            Err(error) => {
                return self
                    .refused(
                        target,
                        keys,
                        &current,
                        oauth::rejection(&error),
                        reactive,
                        &tokens,
                    )
                    .await;
            }
        };
        let fresh = oauth::tokens(&response, Some(&refresh_token));
        let access = fresh.access_token.clone();
        self.publish(
            keys,
            pair::Publish {
                client: &client,
                tokens: &fresh,
                issuer: current.issuer.as_deref(),
                published_by: &format!("{REFRESHED_BY}{}", keys.prefix()),
                obtained_at_ms: self.clock.now_ms(),
                generation: sign_in::random_hex()?,
            },
        )
        .await?;
        tracing::info!(server = %target.server, reactive, "mcp.oauth.refresh.completed");
        Ok(access)
    }

    /// 刷新被拒（Node `handleRefreshFailure`）：确定的拒绝删令牌（比过原文）、要再授权；配置的 client 被拒是配置错误；其余是临时的。
    async fn refused(
        &self,
        target: &McpAuthTarget,
        keys: &Keys,
        current: &Pair,
        rejection: Rejection,
        reactive: bool,
        tokens: &pair::Tokens,
    ) -> Result<RedactedString, McpError> {
        tracing::warn!(server = %target.server, reactive, reason = rejection.message(), "mcp.oauth.refresh.failed");
        match rejection {
            Rejection::Grant(message) => {
                self.invalidate(keys, current.sealed.as_deref(), false)
                    .await?;
                Err(auth_required(message))
            }
            Rejection::Client(_) if target.oauth.client_id.is_some() => Err(McpError::Connect {
                message: format!("MCP server \"{}\": {STATIC_CLIENT_REJECTED}", target.server),
            }),
            Rejection::Client(message) => {
                self.invalidate(keys, current.sealed.as_deref(), true)
                    .await?;
                Err(auth_required(message))
            }
            Rejection::Temporary(message) if reactive => Err(temporary(message)),
            Rejection::Temporary(_) => Ok(tokens.access_token.clone()),
        }
    }
}

#[async_trait]
impl McpTokens for McpOAuthClient {
    async fn token(&self, target: &McpAuthTarget) -> Result<Option<RedactedString>, McpError> {
        let keys = Keys::of(target);
        let Some(tokens) = self
            .pair(&keys)
            .await?
            .and_then(|pair| pair.tokens.map(|tokens| (tokens, pair.expires_at)))
        else {
            return Ok(None);
        };
        let (tokens, expires_at) = tokens;
        // 没有过期时刻（服务器没给 expires_in）时照用，等 401（ext.md §3.12.3）。
        #[expect(clippy::cast_precision_loss, reason = "Unix 毫秒在 2^53 以内")]
        let soon = expires_at.is_some_and(|at| (self.clock.now_ms() + EXPIRY_SKEW_MS) as f64 >= at);
        if !soon || tokens.refresh_token.is_none() {
            return Ok(Some(tokens.access_token));
        }
        self.refresh(target, Some(&tokens.access_token), false)
            .await
            .map(Some)
    }

    async fn refreshed(
        &self,
        target: &McpAuthTarget,
        rejected: Option<&RedactedString>,
    ) -> Result<RedactedString, McpError> {
        self.refresh(target, rejected, true).await
    }
}

#[async_trait]
impl McpAuthorizer for McpOAuthClient {
    async fn begin(
        &self,
        target: &McpAuthTarget,
        scope: Option<&str>,
    ) -> Result<Box<dyn McpPendingSignIn>, McpError> {
        sign_in::begin(self.clone(), target.clone(), scope.map(str::to_owned)).await
    }

    async fn sign_out(&self, target: &McpAuthTarget) -> Result<(), McpError> {
        let keys = Keys::of(target);
        let names = keys.pair();
        let discovery = [keys.key(DISCOVERY), keys.key(DISCOVERY_FETCHED_AT)];
        self.credentials
            .mutate(|credentials| {
                for name in names.iter().chain(&discovery) {
                    credentials.remove(name);
                }
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    async fn signed_in(&self, target: &McpAuthTarget) -> bool {
        let keys = Keys::of(target);
        let [canonical, _, tokens] = keys.pair();
        // 只给界面选 Sign in 还是 Sign in again 用：凭据文件读不出时按没有令牌显示 Sign in（授权时会报出读不出的原因）。
        self.credentials
            .contains_any(&[canonical, tokens])
            .await
            .unwrap_or(false)
    }
}

#[cfg(test)]
#[path = "mcp_oauth_tests.rs"]
mod tests;
