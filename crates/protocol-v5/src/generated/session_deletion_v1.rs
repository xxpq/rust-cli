//! 排版之前的摘要：91a465efd40276b4
//! 生成物，不要手改：`session.deletion@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSessionDataDeletionExecute {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultSessionDataDeletionExecute {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(default = "fallback_command_result_session_data_deletion_execute_status")]
    pub status: CommandResultSessionDataDeletionExecuteStatus,
    #[serde(
        rename = "sessionIds",
        default = "default_command_result_session_data_deletion_execute_session_ids"
    )]
    pub session_ids: Vec<String>,
    #[serde(
        rename = "retainedArtifacts",
        default = "default_command_result_session_data_deletion_execute_retained_artifacts"
    )]
    pub retained_artifacts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CommandResultSessionDataDeletionExecuteStatus {
    Deleted,
    Blocked,
    Incomplete,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl CommandResultSessionDataDeletionExecuteStatus {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Blocked;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Deleted => "deleted",
            Self::Blocked => "blocked",
            Self::Incomplete => "incomplete",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "deleted" => Self::Deleted,
            "blocked" => Self::Blocked,
            "incomplete" => Self::Incomplete,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Blocked,
            known => known.clone(),
        }
    }
}

impl Serialize for CommandResultSessionDataDeletionExecuteStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CommandResultSessionDataDeletionExecuteStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionDataDeletionPreviewParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionDataDeletionPreviewResult {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub revision: String,
    #[serde(
        rename = "sessionIds",
        default = "default_query_session_data_deletion_preview_result_session_ids"
    )]
    pub session_ids: Vec<String>,
    #[serde(default = "default_query_session_data_deletion_preview_result_forks")]
    pub forks: Vec<QuerySessionDataDeletionPreviewResultForksItem>,
    #[serde(
        rename = "retainedArtifacts",
        default = "default_query_session_data_deletion_preview_result_retained_artifacts"
    )]
    pub retained_artifacts: Vec<QuerySessionDataDeletionPreviewResultRetainedArtifactsItem>,
    #[serde(
        rename = "deletedFiles",
        default = "default_query_session_data_deletion_preview_result_deleted_files"
    )]
    pub deleted_files: u64,
    #[serde(
        rename = "blockedReason",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub blocked_reason: Option<QuerySessionDataDeletionPreviewResultBlockedReason>,
    #[serde(default = "default_query_session_data_deletion_preview_result_incomplete")]
    pub incomplete: bool,
    #[serde(default = "default_query_session_data_deletion_preview_result_completed")]
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionDataDeletionPreviewResultForksItem {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(default = "default_query_session_data_deletion_preview_result_forks_item_title")]
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionDataDeletionPreviewResultRetainedArtifactsItem {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "artifactId")]
    pub artifact_id: String,
    #[serde(rename = "fileName")]
    pub file_name: String,
    #[serde(
        default = "default_query_session_data_deletion_preview_result_retained_artifacts_item_referrers"
    )]
    pub referrers: Vec<QuerySessionDataDeletionPreviewResultForksItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QuerySessionDataDeletionPreviewResultBlockedReason {
    Running,
    InUse,
    NotFound,
    NotArchived,
    Unavailable,
    Changed,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QuerySessionDataDeletionPreviewResultBlockedReason {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unavailable;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::InUse => "in-use",
            Self::NotFound => "not-found",
            Self::NotArchived => "not-archived",
            Self::Unavailable => "unavailable",
            Self::Changed => "changed",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "running" => Self::Running,
            "in-use" => Self::InUse,
            "not-found" => Self::NotFound,
            "not-archived" => Self::NotArchived,
            "unavailable" => Self::Unavailable,
            "changed" => Self::Changed,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Unavailable,
            known => known.clone(),
        }
    }
}

impl Serialize for QuerySessionDataDeletionPreviewResultBlockedReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QuerySessionDataDeletionPreviewResultBlockedReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

fn fallback_command_result_session_data_deletion_execute_status()
-> CommandResultSessionDataDeletionExecuteStatus {
    CommandResultSessionDataDeletionExecuteStatus::Blocked
}

fn default_command_result_session_data_deletion_execute_session_ids() -> Vec<String> {
    Vec::new()
}

fn default_command_result_session_data_deletion_execute_retained_artifacts() -> u64 {
    0
}

fn default_query_session_data_deletion_preview_result_session_ids() -> Vec<String> {
    Vec::new()
}

fn default_query_session_data_deletion_preview_result_forks_item_title() -> String {
    String::new()
}

fn default_query_session_data_deletion_preview_result_forks()
-> Vec<QuerySessionDataDeletionPreviewResultForksItem> {
    Vec::new()
}

fn default_query_session_data_deletion_preview_result_retained_artifacts_item_referrers()
-> Vec<QuerySessionDataDeletionPreviewResultForksItem> {
    Vec::new()
}

fn default_query_session_data_deletion_preview_result_retained_artifacts()
-> Vec<QuerySessionDataDeletionPreviewResultRetainedArtifactsItem> {
    Vec::new()
}

fn default_query_session_data_deletion_preview_result_deleted_files() -> u64 {
    0
}

fn default_query_session_data_deletion_preview_result_incomplete() -> bool {
    false
}

fn default_query_session_data_deletion_preview_result_completed() -> bool {
    false
}
