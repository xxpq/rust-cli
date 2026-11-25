//! 排版之前的摘要：e273349acea2a337
//! 生成物，不要手改：`core@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults::{
    catch_protocol_error_message, catch_protocol_error_retryable,
    default_initialize_result_server_build, default_initialize_result_server_kind,
    default_subscribe_cursor, fallback_protocol_error_message, fallback_protocol_error_retryable,
};
use super::{CommandTarget, Cursor, InitializeParamsPeerBuild};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeResultLimits {
    #[serde(rename = "maxMessageBytes")]
    pub max_message_bytes: u64,
    #[serde(rename = "initialCredit")]
    pub initial_credit: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeResultServer {
    #[serde(default = "default_initialize_result_server_kind")]
    pub kind: String,
    #[serde(default = "default_initialize_result_server_build")]
    pub build: InitializeParamsPeerBuild,
    #[serde(rename = "hostId", default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: String,
    #[serde(
        default = "fallback_protocol_error_message",
        deserialize_with = "catch_protocol_error_message"
    )]
    pub message: String,
    #[serde(
        default = "fallback_protocol_error_retryable",
        deserialize_with = "catch_protocol_error_retryable"
    )]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ProtocolErrorOutcome>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProtocolErrorOutcome {
    NotApplied,
    Unknown,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl ProtocolErrorOutcome {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unknown;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::NotApplied => "not_applied",
            Self::Unknown => "unknown",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "not_applied" => Self::NotApplied,
            "unknown" => Self::Unknown,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Unknown,
            known => known.clone(),
        }
    }
}

impl Serialize for ProtocolErrorOutcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProtocolErrorOutcome {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryEnvelope {
    pub r#type: String,
    pub target: CommandTarget,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Opaque>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum QueryResult {
    #[serde(rename = "ok")]
    Ok(QueryResultOk),
    #[serde(rename = "stale")]
    Stale(QueryResultStale),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryResultOk {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub data: Opaque,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryResultStale {
    pub cursor: Cursor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceEnvelope {
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ServiceEnvelopeTarget>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceEnvelopeTarget {
    #[serde(rename = "workspaceKey")]
    pub workspace_key: String,
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscribe {
    pub topic: String,
    pub sub: String,
    #[serde(default = "default_subscribe_cursor")]
    pub cursor: Option<Cursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscribed {
    pub sub: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<SubscribedPending>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SubscribedPending {
    SourceUnavailable,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SubscribedPending {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::SourceUnavailable;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::SourceUnavailable => "source_unavailable",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "source_unavailable" => Self::SourceUnavailable,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::SourceUnavailable,
            known => known.clone(),
        }
    }
}

impl Serialize for SubscribedPending {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SubscribedPending {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionFailed {
    pub sub: String,
    pub error: ProtocolError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unsubscribe {
    pub sub: String,
}

pub type Delta = Opaque;
