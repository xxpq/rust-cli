//! 交互授权（docs/specs/rust-cli/ext.md §3.12.6）：发现 → scope → 本机回调的监听 → client（配置的或动态注册的）→ 授权地址；
//! 回调回来之后换令牌、发布。只由用户的动作发起；放下进行中的授权就关掉监听。
//!
//! 照 TS SDK 的 `auth()`（`determineScope`、`registerClient`、`startAuthorization`、`exchangeAuthorization`）与 Node 的回调
//! （`localhost-callback.ts`）。
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use oauth2::basic::BasicTokenResponse;
use oauth2::{
    AuthorizationCode, CsrfToken, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
};
use rust_cli_base::js::uri::encode_uri_component;
use rust_cli_base::proto::http as status;
use rust_cli_kernel_api::mcp::McpError;
use rust_cli_kernel_api::mcp_auth::{McpAuthTarget, McpPendingSignIn, PastedCallback};
use rust_cli_net::loopback::LoopbackListener;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use url::Url;

use super::discovery::Discovery;
use super::keys::Keys;
use super::oauth::{self, DEFAULT_REGISTER};
use super::{McpOAuthClient, http, pair, temporary};
use crate::credentials::random_bytes;

/// 等浏览器回来最多多久（同 Node）：只等“人在浏览器里点”这一段，换令牌不与它比。
const CALLBACK_WAIT: Duration = Duration::from_secs(300);
/// 缺省的回调路径（Node `normalizeCallbackPath`），后接 `encodeURIComponent(服务器名)`。
const CALLBACK_PATH: &str = "/oauth/callback/mcp/";
/// 回调地址的主机（RFC 8252 §7.3 的回环 IP 字面量，同 Node）。
const CALLBACK_HOST: &str = "http://127.0.0.1";
/// 配了固定回调端口时的回调地址的主机与路径（`http://localhost:<端口>/callback`，ext.md §3.12.15）：授权服务器上登记的是它。
const FIXED_CALLBACK_HOST: &str = "http://localhost";
const FIXED_CALLBACK_PATH: &str = "/callback";
/// 回调页的文字（Node `localhost-callback.ts`）。
const SUCCESS_TEXT: &str =
    "Authorization successful! You may close this window and return to the CLI.";
const FAILURE_TEXT: &str = "Authorization failed. You may close this window and return to the CLI.";
/// 注册时的 client 名：配置没写时 `ZCode <服务器名>`（Node 的 `clientMetadata`）。
const CLIENT_NAME_PREFIX: &str = "ZCode ";
/// 授权服务器支持时加进 scope（TS SDK `determineScope`），加了就要 `prompt=consent`（`startAuthorization`）。
const OFFLINE_ACCESS: &str = "offline_access";
const CONSENT: &str = "consent";
/// PKCE verifier 与 state 的随机字节数（43 个 base64url 字符）。
const RANDOM_BYTES: usize = 32;
/// generation 的随机字节数（Node `createCredentialGeneration` 的 16 字节，32 个十六进制字符）。
const GENERATION_BYTES: usize = 16;
const BASIC: &str = "client_secret_basic";
const NONE: &str = "none";

/// 16 个随机字节的十六进制（每次发布的 generation）。
pub(super) fn random_hex() -> Result<String, McpError> {
    random_bytes::<GENERATION_BYTES>()
        .map(|bytes| rust_cli_base::hash::hex(&bytes))
        .map_err(|error| temporary(error.to_string()))
}

fn random_text() -> Result<String, McpError> {
    random_bytes::<RANDOM_BYTES>()
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|error| temporary(error.to_string()))
}

fn connect(message: impl Into<String>) -> McpError {
    temporary(message)
}

/// 回调路径：配置的 `redirectPath`（没有前导 `/` 时补上）；配了固定回调端口时是 `/callback`；否则缺省的。
fn callback_path(target: &McpAuthTarget) -> String {
    match (&target.oauth.redirect_path, target.oauth.callback_port) {
        (Some(path), _) if path.starts_with('/') => path.clone(),
        (Some(path), _) => format!("/{path}"),
        (None, Some(_)) => FIXED_CALLBACK_PATH.to_owned(),
        (None, None) => format!("{CALLBACK_PATH}{}", encode_uri_component(&target.server)),
    }
}

/// 配了 `authServerMetadataUrl`、没配 scope 时请求的 scope（ext.md §3.12.15）：元数据的 `scope`、
/// `default_scope`，都没有时 `scopes_supported` 连起来。
fn metadata_scope(target: &McpAuthTarget, found: &Discovery) -> Option<String> {
    target.oauth.metadata_url.as_ref()?;
    found
        .metadata_str("scope")
        .or_else(|| found.metadata_str("default_scope"))
        .map(str::to_owned)
        .or_else(|| {
            let supported = found.metadata_list("scopes_supported");
            (!supported.is_empty()).then(|| supported.join(" "))
        })
}

/// scope（TS SDK `determineScope`，Node 把配置的 scope 当作请求的；step-up 时并上现有令牌的与服务器要的）。
fn scope_of(
    target: &McpAuthTarget,
    found: &Discovery,
    granted: Option<&str>,
    step_up: Option<&str>,
) -> Option<String> {
    let requested = match step_up {
        Some(extra) => {
            let mut union: Vec<&str> = Vec::new();
            for scope in [target.oauth.scope.as_deref(), granted, Some(extra)]
                .into_iter()
                .flatten()
                .flat_map(str::split_whitespace)
            {
                if !union.contains(&scope) {
                    union.push(scope);
                }
            }
            Some(union.join(" ")).filter(|joined| !joined.is_empty())
        }
        None => target.oauth.scope.clone().filter(|scope| !scope.is_empty()),
    };
    let resource = found.resource_scopes();
    let mut effective = requested
        .or_else(|| (!resource.is_empty()).then(|| resource.join(" ")))
        .or_else(|| metadata_scope(target, found))?;
    let offered = found
        .metadata_list("scopes_supported")
        .contains(&OFFLINE_ACCESS);
    if offered
        && !effective
            .split_whitespace()
            .any(|scope| scope == OFFLINE_ACCESS)
    {
        effective = format!("{effective} {OFFLINE_ACCESS}");
    }
    Some(effective)
}

/// client：配置了 `clientId` 用它；否则动态注册（RFC 7591，TS SDK `registerClient`）。注册的回复原样存（可能带 secret）。
async fn client_for(
    oauth_client: &McpOAuthClient,
    target: &McpAuthTarget,
    found: &Discovery,
    redirect: &str,
    scope: Option<&str>,
) -> Result<Map<String, Value>, McpError> {
    let oauth = &target.oauth;
    if let Some(id) = &oauth.client_id {
        let mut client = Map::from_iter([("client_id".to_owned(), Value::String(id.clone()))]);
        if let Some(secret) = &oauth.client_secret {
            client.insert(
                "client_secret".to_owned(),
                Value::String(secret.expose().to_owned()),
            );
        }
        return Ok(client);
    }
    let endpoint = match (
        &found.authorization_server_metadata,
        found.metadata_str("registration_endpoint"),
    ) {
        (_, Some(endpoint)) => endpoint.to_owned(),
        (Some(_), None) => {
            return Err(connect(
                "Incompatible auth server: does not support dynamic client registration",
            ));
        }
        (None, None) => found
            .endpoint("registration_endpoint", DEFAULT_REGISTER)
            .map_err(connect)?,
    };
    let name = oauth
        .client_name
        .clone()
        .unwrap_or_else(|| format!("{CLIENT_NAME_PREFIX}{}", target.server));
    let mut body = json!({
        "client_name": name,
        "grant_types": ["authorization_code", "refresh_token"],
        "redirect_uris": [redirect],
        "response_types": ["code"],
        "token_endpoint_auth_method": if oauth.client_secret.is_some() { BASIC } else { NONE },
    });
    if let Some(scope) = scope {
        body["scope"] = Value::String(scope.to_owned());
    }
    let reply = http::post_json(&oauth_client.egress, &endpoint, &body)
        .await
        .map_err(|error| connect(error.0))?;
    match reply.body {
        Some(Value::Object(client))
            if reply.ok() && client.get("client_id").is_some_and(Value::is_string) =>
        {
            Ok(client)
        }
        _ => Err(connect(format!(
            "Dynamic client registration failed: HTTP {}: {}",
            reply.status, reply.text
        ))),
    }
}

/// 开始一次授权（§3.12.6 第 1–5 步）。
pub(super) async fn begin(
    oauth_client: McpOAuthClient,
    target: McpAuthTarget,
    step_up: Option<String>,
) -> Result<Box<dyn McpPendingSignIn>, McpError> {
    let keys = Keys::of(&target);
    let current = oauth_client.pair(&keys).await?;
    let issuer = current.as_ref().and_then(|pair| pair.issuer.clone());
    let found = oauth_client
        .discovery(&keys, &target, issuer.as_deref())
        .await
        .map_err(connect)?;
    let resource = found.resource(&target.url).map_err(connect)?;
    let granted = current
        .as_ref()
        .and_then(|pair| pair.tokens.as_ref())
        .and_then(|tokens| tokens.scope.clone());
    let scope = scope_of(&target, &found, granted.as_deref(), step_up.as_deref());
    let listener = match target.oauth.callback_port {
        Some(port) => LoopbackListener::bind_port(port).await,
        None => LoopbackListener::bind().await,
    }
    .map_err(|error| connect(format!("could not listen for the OAuth callback: {error}")))?;
    let path = callback_path(&target);
    let host = if target.oauth.callback_port.is_some() {
        FIXED_CALLBACK_HOST
    } else {
        CALLBACK_HOST
    };
    let redirect = format!("{host}:{}{path}", listener.port());
    let client = client_for(&oauth_client, &target, &found, &redirect, scope.as_deref()).await?;
    let built = oauth::client(&client, &found)
        .map_err(connect)?
        .set_redirect_uri(
            RedirectUrl::new(redirect.clone()).map_err(|error| connect(error.to_string()))?,
        );
    let verifier = PkceCodeVerifier::new(random_text()?);
    let challenge = PkceCodeChallenge::from_code_verifier_sha256(&verifier);
    let state = random_text()?;
    let mut request = built
        .authorize_url(|| CsrfToken::new(state.clone()))
        .set_pkce_challenge(challenge);
    if let Some(scope) = &scope {
        request = request.add_scope(Scope::new(scope.clone()));
        if scope.split_whitespace().any(|item| item == OFFLINE_ACCESS) {
            request = request.add_extra_param("prompt", CONSENT);
        }
    }
    if let Some(resource) = &resource {
        request = request.add_extra_param("resource", resource);
    }
    let (url, _) = request.url();
    tracing::info!(server = %target.server, port = listener.port(), "mcp.oauth.authorization.started");
    Ok(Box::new(Pending {
        client: oauth_client,
        target,
        keys,
        found,
        resource,
        authorization: client,
        redirect,
        path,
        state,
        verifier,
        listener,
        url: url.to_string(),
    }))
}

struct Pending {
    client: McpOAuthClient,
    target: McpAuthTarget,
    keys: Keys,
    found: Discovery,
    resource: Option<String>,
    authorization: Map<String, Value>,
    redirect: String,
    path: String,
    state: String,
    verifier: PkceCodeVerifier,
    listener: LoopbackListener,
    url: String,
}

/// 回调里的 code 与 `iss`（RFC 9207）。
struct Callback {
    code: String,
    issuer: Option<String>,
}

/// 回调地址里的参数。
fn query(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

impl Pending {
    /// 等这次授权的回调（Node `createLocalhostOAuthCallbackServer`）：路径不对 404；state 不对 400、接着等（陌生的 state 不能
    /// 打断正确的那个）；有 `error` 立即失败；没有 code 失败。用户贴回来的地址（`pasted`，ext.md §3.12.16）：state 是这一次的、带
    /// `code` 或 `error` 才收下（当场回答），收下的同回调；不收的不打断，接着等。
    async fn callback(
        &self,
        pasted: &mut mpsc::Receiver<PastedCallback>,
    ) -> Result<Callback, McpError> {
        loop {
            let request = tokio::select! {
                accepted = self.listener.accept() => {
                    accepted.map_err(|error| connect(format!("OAuth callback: {error}")))?
                }
                Some(callback) = pasted.recv() => {
                    let parsed = Url::parse(&callback.url).ok().filter(|url| {
                        query(url, "state").as_deref() == Some(self.state.as_str())
                            && (query(url, "code").is_some() || query(url, "error").is_some())
                    });
                    let _ = callback.accepted.send(parsed.is_some());
                    match parsed {
                        Some(url) => return Self::arrived(&url),
                        None => continue,
                    }
                }
            };
            let parsed = Url::parse(&format!("{CALLBACK_HOST}{}", request.target)).ok();
            let Some(parsed) = parsed.filter(|url| url.path() == self.path) else {
                request.respond(status::NOT_FOUND, FAILURE_TEXT).await;
                continue;
            };
            if query(&parsed, "state").as_deref() != Some(self.state.as_str()) {
                request.respond(status::BAD_REQUEST, FAILURE_TEXT).await;
                continue;
            }
            let arrived = Self::arrived(&parsed);
            let page = if arrived.is_ok() {
                (status::OK, SUCCESS_TEXT)
            } else {
                (status::BAD_REQUEST, FAILURE_TEXT)
            };
            request.respond(page.0, page.1).await;
            return arrived;
        }
    }

    /// state 对了的一次回调：有 `error` 立即失败；没有 code 失败；否则 code 与 `iss`。
    fn arrived(url: &Url) -> Result<Callback, McpError> {
        if let Some(error) = query(url, "error") {
            return Err(connect(format!(
                "OAuth authorization was rejected by the authorization server: {error}"
            )));
        }
        let Some(code) = query(url, "code").filter(|code| !code.is_empty()) else {
            return Err(connect("OAuth callback is missing an authorization code."));
        };
        Ok(Callback {
            code,
            issuer: query(url, "iss"),
        })
    }

    /// RFC 9207（TS SDK `validateAuthorizationResponseIssuer`）：服务器声明会带 `iss` 时必须有；有就要等于元数据的 issuer。
    fn check_issuer(&self, issuer: Option<&str>) -> Result<(), McpError> {
        let supported = self
            .found
            .authorization_server_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("authorization_response_iss_parameter_supported"))
            .and_then(Value::as_bool)
            == Some(true);
        let expected = self.found.metadata_str("issuer");
        match (issuer, expected) {
            (None, _) if supported => {
                Err(connect("the authorization response has no iss parameter"))
            }
            (Some(got), Some(expected)) if got != expected => Err(connect(format!(
                "the authorization response's issuer {got} is not {expected}"
            ))),
            _ => Ok(()),
        }
    }

    async fn exchange(&self, code: String) -> Result<BasicTokenResponse, McpError> {
        let built = oauth::client(&self.authorization, &self.found)
            .map_err(connect)?
            .set_redirect_uri(
                RedirectUrl::new(self.redirect.clone())
                    .map_err(|error| connect(error.to_string()))?,
            );
        let mut request = built
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(PkceCodeVerifier::new(self.verifier.secret().clone()));
        if let Some(resource) = &self.resource {
            request = request.add_extra_param("resource", resource);
        }
        request
            .request_async(&http::EgressClient(&self.client.egress))
            .await
            .map_err(|error| connect(oauth::rejection(&error).message().to_owned()))
    }
}

#[async_trait]
impl McpPendingSignIn for Pending {
    fn url(&self) -> &str {
        &self.url
    }

    async fn finish(
        self: Box<Self>,
        mut pasted: mpsc::Receiver<PastedCallback>,
    ) -> Result<(), McpError> {
        let callback = tokio::time::timeout(CALLBACK_WAIT, self.callback(&mut pasted))
            .await
            .map_err(|_| {
                connect(format!(
                    "MCP server {} OAuth authorization timed out",
                    self.target.server
                ))
            })??;
        self.check_issuer(callback.issuer.as_deref())?;
        // 拿到 code 之后一定做完换令牌（不与等待的期限比，Node mcp-oauth-two-phase.md §4.6）。
        let response = self.exchange(callback.code).await?;
        let tokens = oauth::tokens(&response, None);
        let published_by = rust_cli_base::hash::sha256_hex(self.state.as_bytes());
        self.client
            .publish(
                &self.keys,
                pair::Publish {
                    client: &self.authorization,
                    tokens: &tokens,
                    issuer: self.found.metadata_str("issuer"),
                    published_by: &published_by,
                    obtained_at_ms: self.client.clock.now_ms(),
                    generation: random_hex()?,
                },
            )
            .await?;
        tracing::info!(server = %self.target.server, "mcp.oauth.authorization.completed");
        Ok(())
    }
}

#[cfg(test)]
#[path = "sign_in_tests.rs"]
mod tests;
