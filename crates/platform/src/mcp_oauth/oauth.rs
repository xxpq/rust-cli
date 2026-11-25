//! `oauth2` 的客户端（docs/specs/rust-cli/ext.md §3.12.12）：按存下的 client 与授权服务器的元数据建，换令牌与刷新共用；回复换成
//! Node 存的 [`Tokens`]；拒绝按确定与否分类（§3.12.4 的失效、§3.12.5）。
use oauth2::basic::{BasicClient, BasicErrorResponseType, BasicTokenResponse};
use oauth2::{
    AuthType, AuthUrl, ClientId, ClientSecret, EndpointNotSet, EndpointSet, RequestTokenError,
    StandardErrorResponse, TokenResponse, TokenUrl,
};
use rust_cli_base::secret::RedactedString;
use serde_json::{Map, Value};

use super::discovery::Discovery;
use super::http::HttpFailure;
use super::pair::Tokens;

/// 没有元数据时的端点（TS SDK 的缺省路径）。
pub(super) const DEFAULT_AUTHORIZE: &str = "/authorize";
pub(super) const DEFAULT_TOKEN: &str = "/token";
pub(super) const DEFAULT_REGISTER: &str = "/register";

/// client 认证的方法（TS SDK `selectClientAuthMethod` 的结果）。
const BASIC: &str = "client_secret_basic";
const POST: &str = "client_secret_post";
const NONE: &str = "none";

/// 非标准、但意思同 `invalid_grant` 的错误码（ext.md §3.12.5）。
const GRANT_ALIASES: [&str; 3] = [
    "invalid_refresh_token",
    "expired_refresh_token",
    "token_expired",
];

pub(super) type Client =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;
type Refused = RequestTokenError<HttpFailure, StandardErrorResponse<BasicErrorResponseType>>;

/// 授权服务器怎样拒绝的（刷新与换令牌）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Rejection {
    /// refresh token 失效（`invalid_grant` 与它的别名）：删令牌、要再授权。
    Grant(String),
    /// client 不被认（`invalid_client`、`unauthorized_client`）。
    Client(String),
    /// 网络、5xx、别的错误：临时的，不删凭据。
    Temporary(String),
}

impl Rejection {
    pub(super) fn message(&self) -> &str {
        match self {
            Self::Grant(message) | Self::Client(message) | Self::Temporary(message) => message,
        }
    }
}

/// 存下的 client 的一个字符串字段。
fn field<'a>(client: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    client.get(name).and_then(Value::as_str)
}

/// TS SDK `selectClientAuthMethod`：client 自己声明的、授权服务器支持的方法 → 有 secret 用 basic 或 post → none。
fn auth_method<'a>(client: &'a Map<String, Value>, supported: &[&'a str]) -> &'a str {
    let secret = field(client, "client_secret").is_some();
    if let Some(declared) = field(client, "token_endpoint_auth_method")
        .filter(|method| [BASIC, POST, NONE].contains(method))
        .filter(|method| supported.is_empty() || supported.contains(method))
    {
        return declared;
    }
    match (secret, supported.is_empty()) {
        (true, true) => BASIC,
        (false, true) => NONE,
        (true, false) if supported.contains(&BASIC) => BASIC,
        (true, false) if supported.contains(&POST) => POST,
        _ if supported.contains(&NONE) => NONE,
        (true, false) => POST,
        (false, false) => NONE,
    }
}

/// 按存下的 client 与发现的元数据建客户端（授权地址与令牌端点）。
pub(super) fn client(client: &Map<String, Value>, discovery: &Discovery) -> Result<Client, String> {
    let id = field(client, "client_id").ok_or("the stored OAuth client has no client_id")?;
    let supported = discovery.metadata_list("token_endpoint_auth_methods_supported");
    let method = auth_method(client, &supported);
    let auth = discovery.endpoint("authorization_endpoint", DEFAULT_AUTHORIZE)?;
    let token = discovery.endpoint("token_endpoint", DEFAULT_TOKEN)?;
    let built = BasicClient::new(ClientId::new(id.to_owned()))
        .set_auth_uri(AuthUrl::new(auth).map_err(|error| error.to_string())?)
        .set_token_uri(TokenUrl::new(token).map_err(|error| error.to_string())?)
        .set_auth_type(if method == POST {
            AuthType::RequestBody
        } else {
            AuthType::BasicAuth
        });
    // `none`：只在正文里带 client_id（oauth2 没有 secret 时就这样发）。
    Ok(
        match field(client, "client_secret").filter(|_| method != NONE) {
            Some(secret) => built.set_client_secret(ClientSecret::new(secret.to_owned())),
            None => built,
        },
    )
}

/// 令牌端点的回复 → Node 存的形状。刷新时服务器没给新的 refresh token 就留旧的（TS SDK `refreshAuthorization`）。
pub(super) fn tokens(response: &BasicTokenResponse, kept: Option<&RedactedString>) -> Tokens {
    let refresh = response
        .refresh_token()
        .map(|token| RedactedString::new(token.secret()))
        .or_else(|| kept.cloned());
    Tokens {
        access_token: RedactedString::new(response.access_token().secret()),
        id_token: None,
        token_type: token_type(response),
        expires_in: response.expires_in().map(|left| left.as_secs().into()),
        scope: response.scopes().map(|scopes| {
            scopes
                .iter()
                .map(|scope| scope.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        }),
        refresh_token: refresh,
    }
}

fn token_type(response: &BasicTokenResponse) -> String {
    match serde_json::to_value(response.token_type()) {
        Ok(Value::String(kind)) => kind,
        _ => "bearer".to_owned(),
    }
}

/// 换令牌与刷新的失败怎样算（§3.12.5）。
pub(super) fn rejection(error: &Refused) -> Rejection {
    match error {
        RequestTokenError::ServerResponse(response) => {
            let code = response.error().as_ref();
            let text = match response.error_description() {
                Some(description) => format!("{code}: {description}"),
                None => code.to_owned(),
            };
            match response.error() {
                BasicErrorResponseType::InvalidGrant => Rejection::Grant(text),
                BasicErrorResponseType::InvalidClient
                | BasicErrorResponseType::UnauthorizedClient => Rejection::Client(text),
                BasicErrorResponseType::Extension(other)
                    if GRANT_ALIASES.contains(&other.as_str()) =>
                {
                    Rejection::Grant(text)
                }
                _ => Rejection::Temporary(text),
            }
        }
        other => Rejection::Temporary(other.to_string()),
    }
}
