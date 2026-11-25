//! 排版之前的摘要：279c53d67b17cec1
//! 生成物，不要手改：`mcp.oauth@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandMcpOauthSignIn {
    #[serde(rename = "serverName")]
    pub server_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultMcpOauthSignIn {
    #[serde(rename = "authorizationUrl")]
    pub authorization_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultMcpOauthSignOut {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryMcpOauthServersResult {
    #[serde(default = "default_query_mcp_oauth_servers_result_servers")]
    pub servers: Vec<QueryMcpOauthServersResultServersItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryMcpOauthServersResultServersItem {
    pub name: String,
    #[serde(rename = "signedIn")]
    pub signed_in: bool,
    #[serde(rename = "needsAuth")]
    pub needs_auth: bool,
    #[serde(
        rename = "pendingAuthorizationUrl",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub pending_authorization_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryMcpOauthSignInOutcomeParams {
    #[serde(rename = "serverName")]
    pub server_name: String,
    #[serde(rename = "authorizationUrl")]
    pub authorization_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryMcpOauthSignInOutcomeResult {
    #[serde(default = "fallback_query_mcp_oauth_sign_in_outcome_result_outcome")]
    pub outcome: QueryMcpOauthSignInOutcomeResultOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QueryMcpOauthSignInOutcomeResultOutcome {
    SignedIn,
    Failed,
    Replaced,
    Ended,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QueryMcpOauthSignInOutcomeResultOutcome {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Ended;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::SignedIn => "signedIn",
            Self::Failed => "failed",
            Self::Replaced => "replaced",
            Self::Ended => "ended",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "signedIn" => Self::SignedIn,
            "failed" => Self::Failed,
            "replaced" => Self::Replaced,
            "ended" => Self::Ended,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Ended,
            known => known.clone(),
        }
    }
}

impl Serialize for QueryMcpOauthSignInOutcomeResultOutcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QueryMcpOauthSignInOutcomeResultOutcome {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

pub type CommandMcpOauthSignOut = CommandMcpOauthSignIn;

pub type QueryMcpOauthServersParams = CommandResultMcpOauthSignOut;

fn default_query_mcp_oauth_servers_result_servers() -> Vec<QueryMcpOauthServersResultServersItem> {
    Vec::new()
}

fn fallback_query_mcp_oauth_sign_in_outcome_result_outcome()
-> QueryMcpOauthSignInOutcomeResultOutcome {
    QueryMcpOauthSignInOutcomeResultOutcome::Ended
}
