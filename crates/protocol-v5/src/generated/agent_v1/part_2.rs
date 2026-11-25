//! 排版之前的摘要：f00f9309ee521c14
//! 生成物，不要手改：`agent@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults::default_agent_state_diagnostics_version;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentStateStorageMigrationKind {
    None,
    Initialize,
    Upgrade,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl AgentStateStorageMigrationKind {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::None;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Initialize => "initialize",
            Self::Upgrade => "upgrade",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "none" => Self::None,
            "initialize" => Self::Initialize,
            "upgrade" => Self::Upgrade,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::None,
            known => known.clone(),
        }
    }
}

impl Serialize for AgentStateStorageMigrationKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentStateStorageMigrationKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStateDiagnostics {
    pub implementation: String,
    #[serde(default = "default_agent_state_diagnostics_version")]
    pub version: String,
    #[serde(
        rename = "protocolDigest",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub protocol_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAgentRelease {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAgentRestart {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultAgentRelease {
    pub released: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultAgentRestart {}
