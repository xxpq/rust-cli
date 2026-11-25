//! 排版之前的摘要：ec4eb18c4741a79f
//! 生成物，不要手改：`session.purge@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSessionDataPurge {
    #[serde(rename = "sessionIds")]
    pub session_ids: Vec<String>,
    #[serde(default = "default_command_session_data_purge_memory")]
    pub memory: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultSessionDataPurge {
    #[serde(default = "default_command_result_session_data_purge_deleted")]
    pub deleted: Vec<CommandResultSessionDataPurgeDeletedItem>,
    #[serde(default = "default_command_result_session_data_purge_kept")]
    pub kept: Vec<CommandResultSessionDataPurgeKeptItem>,
    #[serde(default = "default_command_result_session_data_purge_failed")]
    pub failed: Vec<CommandResultSessionDataPurgeFailedItem>,
    #[serde(
        rename = "memoryBytes",
        default = "default_command_result_session_data_purge_memory_bytes"
    )]
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultSessionDataPurgeDeletedItem {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultSessionDataPurgeKeptItem {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(default = "fallback_command_result_session_data_purge_kept_item_reason")]
    pub reason: CommandResultSessionDataPurgeKeptItemReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CommandResultSessionDataPurgeKeptItemReason {
    Referenced,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl CommandResultSessionDataPurgeKeptItemReason {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Referenced;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Referenced => "referenced",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "referenced" => Self::Referenced,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Referenced,
            known => known.clone(),
        }
    }
}

impl Serialize for CommandResultSessionDataPurgeKeptItemReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CommandResultSessionDataPurgeKeptItemReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultSessionDataPurgeFailedItem {
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionDataMeasureParams {
    #[serde(rename = "sessionIds")]
    pub session_ids: Vec<String>,
    #[serde(default = "default_query_session_data_measure_params_memory")]
    pub memory: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionDataMeasureResult {
    #[serde(default = "default_query_session_data_measure_result_sessions")]
    pub sessions: Vec<CommandResultSessionDataPurgeDeletedItem>,
    #[serde(
        rename = "memoryBytes",
        default = "default_query_session_data_measure_result_memory_bytes"
    )]
    pub memory_bytes: u64,
}

fn default_command_session_data_purge_memory() -> bool {
    false
}

fn default_command_result_session_data_purge_deleted()
-> Vec<CommandResultSessionDataPurgeDeletedItem> {
    Vec::new()
}

fn fallback_command_result_session_data_purge_kept_item_reason()
-> CommandResultSessionDataPurgeKeptItemReason {
    CommandResultSessionDataPurgeKeptItemReason::Referenced
}

fn default_command_result_session_data_purge_kept() -> Vec<CommandResultSessionDataPurgeKeptItem> {
    Vec::new()
}

fn default_command_result_session_data_purge_failed() -> Vec<CommandResultSessionDataPurgeFailedItem>
{
    Vec::new()
}

fn default_command_result_session_data_purge_memory_bytes() -> u64 {
    0
}

fn default_query_session_data_measure_params_memory() -> bool {
    false
}

fn default_query_session_data_measure_result_sessions()
-> Vec<CommandResultSessionDataPurgeDeletedItem> {
    Vec::new()
}

fn default_query_session_data_measure_result_memory_bytes() -> u64 {
    0
}
