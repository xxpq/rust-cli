//! 发现（docs/specs/rust-cli/ext.md §3.12.6 第 1 步）：受保护资源元数据（RFC 9728）→ 授权服务器 → 授权服务器元数据（RFC 8414 /
//! OIDC）→ resource（RFC 8707）。URL 的拼法、回落、issuer 的检查逐条照 TS SDK（`discoverOAuthServerInfo`、
//! `discoverMetadataWithFallback`、`buildDiscoveryUrls`、`discoverAuthorizationServerMetadata`、`selectResourceURL`、
//! `checkResourceAllowed`），Node 用的是同一份。
use rust_cli_base::proto::http as status;
use rust_cli_net::Egress;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use super::http::{HttpFailure, get_json};

const PROTECTED_RESOURCE: &str = "oauth-protected-resource";
const AUTHORIZATION_SERVER: &str = "oauth-authorization-server";
const OPENID_CONFIGURATION: &str = "openid-configuration";
const WELL_KNOWN: &str = "/.well-known/";

/// 发现的结果（TS SDK `OAuthDiscoveryState`；存进凭据文件，与 Node 互读，ext.md §3.12.4）。元数据原样存。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Discovery {
    pub authorization_server_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_metadata_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_server_metadata: Option<Value>,
}

impl Discovery {
    /// 授权服务器元数据的一个字符串字段。
    pub(super) fn metadata_str(&self, field: &str) -> Option<&str> {
        self.authorization_server_metadata
            .as_ref()?
            .get(field)?
            .as_str()
    }

    /// 授权服务器元数据的一个字符串数组（没有时空）。
    pub(super) fn metadata_list(&self, field: &str) -> Vec<&str> {
        list(self.authorization_server_metadata.as_ref(), field)
    }

    /// 受保护资源元数据的 `scopes_supported`。
    pub(super) fn resource_scopes(&self) -> Vec<&str> {
        list(self.resource_metadata.as_ref(), "scopes_supported")
    }

    /// 授权服务器的 issuer：元数据的，没有时是授权服务器 URL（Node `issuersMatch`）。
    pub(super) fn issuer(&self) -> &str {
        self.metadata_str("issuer")
            .unwrap_or(&self.authorization_server_url)
    }

    /// 端点：元数据给的，没有元数据时是授权服务器 URL 上的缺省路径（TS SDK：`/authorize`、`/token`、`/register`）。
    pub(super) fn endpoint(&self, field: &str, fallback: &str) -> Result<String, String> {
        if let Some(endpoint) = self.metadata_str(field) {
            return Ok(endpoint.to_owned());
        }
        Url::parse(&self.authorization_server_url)
            .and_then(|base| base.join(fallback))
            .map(String::from)
            .map_err(|error| format!("invalid authorization server URL: {error}"))
    }

    /// RFC 8707 的 resource（TS SDK `selectResourceURL`）：有资源元数据时它的 `resource` 要与服务器 URL 同源、路径是前缀，取它；
    /// 没有资源元数据时不带。
    pub(super) fn resource(&self, server_url: &str) -> Result<Option<String>, String> {
        let Some(configured) = self
            .resource_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("resource"))
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        let mut requested = Url::parse(server_url).map_err(|error| error.to_string())?;
        requested.set_fragment(None);
        let configured_url = Url::parse(configured).map_err(|error| error.to_string())?;
        if !resource_allowed(&requested, &configured_url) {
            return Err(format!(
                "Protected resource {configured} does not match expected {requested} (or origin)"
            ));
        }
        Ok(Some(configured_url.to_string()))
    }
}

fn list<'a>(metadata: Option<&'a Value>, field: &str) -> Vec<&'a str> {
    metadata
        .and_then(|metadata| metadata.get(field))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// TS SDK `checkResourceAllowed`：同源，且请求的路径以配置的路径开头（都按以 `/` 结尾比）。
fn resource_allowed(requested: &Url, configured: &Url) -> bool {
    if requested.origin() != configured.origin() || requested.path().len() < configured.path().len()
    {
        return false;
    }
    let slashed = |path: &str| {
        if path.ends_with('/') {
            path.to_owned()
        } else {
            format!("{path}/")
        }
    };
    slashed(requested.path()).starts_with(&slashed(configured.path()))
}

/// 4xx 与 502：换下一个地址（TS SDK `shouldAttemptFallback` 与 `discoverAuthorizationServerMetadata`）。
fn try_next(code: u16) -> bool {
    status::CLIENT_ERROR.contains(&code) || code == status::BAD_GATEWAY
}

/// TS SDK `buildWellKnownPath`：`/.well-known/<类型><路径>`，路径去掉末尾的 `/`。
fn well_known(kind: &str, path: &str) -> String {
    format!(
        "{WELL_KNOWN}{kind}{}",
        path.strip_suffix('/').unwrap_or(path)
    )
}

/// 现取（TS SDK `discoverOAuthServerInfo`）。受保护资源元数据取不到时授权服务器是服务器 URL 的根；授权服务器元数据取不到时
/// 没有元数据（之后用缺省路径）。
pub(super) async fn discover(
    egress: &Egress,
    server_url: &str,
    metadata_url: Option<&str>,
) -> Result<Discovery, String> {
    if let Some(metadata_url) = metadata_url {
        return configured(egress, metadata_url).await;
    }
    let server =
        Url::parse(server_url).map_err(|error| format!("invalid MCP server URL: {error}"))?;
    let resource_metadata = protected_resource(egress, &server).await.map_err(|e| e.0)?;
    let authorization_server_url = resource_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("authorization_servers"))
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(Value::as_str)
        .map_or_else(
            || server.join("/").map(String::from),
            |first| Ok(first.to_owned()),
        )
        .map_err(|error| error.to_string())?;
    let authorization_server_metadata =
        authorization_server(egress, &authorization_server_url).await?;
    Ok(Discovery {
        authorization_server_url,
        resource_metadata_url: None,
        resource_metadata,
        authorization_server_metadata,
    })
}

/// 配置的 `authServerMetadataUrl`（ext.md §3.12.15）：只取这个地址当授权服务器元数据，不做受保护资源与按路径的发现；
/// 授权服务器 = 元数据的 `issuer`（没有时是这个地址的根），不查 issuer；取不到就失败、不回落。
async fn configured(egress: &Egress, metadata_url: &str) -> Result<Discovery, String> {
    let reply = get_json(egress, metadata_url).await.map_err(|e| e.0)?;
    if !reply.ok() {
        return Err(format!(
            "HTTP {} trying to load authorization server metadata from {metadata_url}",
            reply.status
        ));
    }
    let Some(metadata) = reply.body.filter(Value::is_object) else {
        return Err(format!(
            "invalid authorization server metadata from {metadata_url}"
        ));
    };
    let authorization_server_url = match metadata.get("issuer").and_then(Value::as_str) {
        Some(issuer) => issuer.to_owned(),
        None => Url::parse(metadata_url)
            .and_then(|url| url.join("/"))
            .map(String::from)
            .map_err(|error| format!("invalid authorization server metadata URL: {error}"))?,
    };
    Ok(Discovery {
        authorization_server_url,
        resource_metadata_url: None,
        resource_metadata: None,
        authorization_server_metadata: Some(metadata),
    })
}

/// RFC 9728：`/.well-known/oauth-protected-resource<路径>`（带服务器 URL 的查询串）；路径不是 `/` 且 4xx / 502 时再试根。不成
/// （404、别的状态、不是带 `resource` 的 JSON）时没有。
async fn protected_resource(egress: &Egress, server: &Url) -> Result<Option<Value>, HttpFailure> {
    let mut first = server.clone();
    first.set_path(&well_known(PROTECTED_RESOURCE, server.path()));
    first.set_fragment(None);
    let mut reply = get_json(egress, first.as_str()).await?;
    if server.path() != "/" && try_next(reply.status) {
        let mut root = server.clone();
        root.set_path(&well_known(PROTECTED_RESOURCE, ""));
        root.set_query(None);
        root.set_fragment(None);
        reply = get_json(egress, root.as_str()).await?;
    }
    let ok = reply.ok();
    Ok(reply
        .body
        .filter(|body| ok && body.get("resource").is_some_and(Value::is_string)))
}

/// RFC 8414 与 OIDC 的候选地址（TS SDK `buildDiscoveryUrls`）。
fn metadata_urls(authorization_server: &Url) -> Vec<Url> {
    let path = authorization_server.path();
    let at = |path: &str| {
        let mut url = authorization_server.clone();
        url.set_path(path);
        url.set_query(None);
        url.set_fragment(None);
        url
    };
    if path == "/" {
        return vec![
            at(&well_known(AUTHORIZATION_SERVER, "")),
            at(&well_known(OPENID_CONFIGURATION, "")),
        ];
    }
    let path = path.strip_suffix('/').unwrap_or(path);
    vec![
        at(&well_known(AUTHORIZATION_SERVER, path)),
        at(&well_known(OPENID_CONFIGURATION, path)),
        at(&format!("{path}{WELL_KNOWN}{OPENID_CONFIGURATION}")),
    ]
}

/// 依次试候选地址：4xx、502 换下一个；别的不成功的状态是错误。取到的 `issuer` 要等于授权服务器 URL（可差末尾的 `/`，
/// RFC 8414 §3.3）。都试过了没有时 None。
async fn authorization_server(egress: &Egress, url: &str) -> Result<Option<Value>, String> {
    let parsed =
        Url::parse(url).map_err(|error| format!("invalid authorization server URL: {error}"))?;
    for candidate in metadata_urls(&parsed) {
        let reply = get_json(egress, candidate.as_str())
            .await
            .map_err(|e| e.0)?;
        if !reply.ok() {
            if try_next(reply.status) {
                continue;
            }
            return Err(format!(
                "HTTP {} trying to load authorization server metadata from {candidate}",
                reply.status
            ));
        }
        let Some(metadata) = reply.body.filter(Value::is_object) else {
            return Err(format!(
                "invalid authorization server metadata from {candidate}"
            ));
        };
        let issuer = metadata
            .get("issuer")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let matches = issuer == url || url.strip_suffix('/') == Some(issuer);
        if !matches {
            return Err(format!(
                "authorization server metadata issuer {issuer} does not match {url}"
            ));
        }
        return Ok(Some(metadata));
    }
    Ok(None)
}

/// 缓存的发现结果还能不能用（Node `loadDiscoveryRecord`）：取得不到 24 小时，且 issuer 与 canonical 记下的相同（都去掉末尾的
/// `/` 比）。缺时间、过期、解不开时现取。
pub(super) fn cached(
    stored: Option<&str>,
    fetched_at: Option<&str>,
    now_ms: u64,
    expected_issuer: Option<&str>,
) -> Option<Discovery> {
    let fetched_at: u64 = fetched_at?.trim().parse().ok()?;
    if now_ms.saturating_sub(fetched_at) >= super::DISCOVERY_TTL_MS {
        return None;
    }
    let discovery: Discovery = serde_json::from_str(stored?).ok()?;
    let trim = |issuer: &str| issuer.strip_suffix('/').unwrap_or(issuer).to_owned();
    match expected_issuer {
        Some(expected) if trim(discovery.issuer()) != trim(expected) => None,
        _ => Some(discovery),
    }
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
