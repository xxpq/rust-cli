//! 排版之前的摘要：e62a5488b138a53b
//! 生成物，不要手改：`session@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults_1::{
    default_query_session_debug_result_network_entries_item_request_headers,
    default_query_session_debug_result_network_entries_item_response_headers,
    default_query_session_file_changes_result_items_item_patches,
    default_query_session_file_changes_result_items_item_tool_names,
    default_query_session_prompts_result_items_item_answer,
    default_query_session_subagents_params_ended_limit, default_session_fact_fields_config,
    default_session_fact_fields_config_mode, default_session_fact_fields_config_model,
    default_session_fact_fields_config_thought, default_session_fact_fields_created_at,
    default_session_fact_fields_deleted_at, default_session_fact_fields_last_outcome,
    default_session_fact_fields_title, default_session_fact_fields_title_source,
    default_session_fact_fields_updated_at,
    fallback_query_session_debug_result_network_entries_item_status_type,
    fallback_query_session_subagents_result_ended_items_item_status,
    fallback_query_session_subagents_result_running_item_status,
};
use super::{Row, RowTurnHeaderFileChangeSummaryState};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionDebugResultNetworkEntriesItem {
    #[serde(rename = "eventKey")]
    pub event_key: String,
    #[serde(rename = "traceId")]
    pub trace_id: String,
    #[serde(rename = "recordedAt")]
    pub recorded_at: f64,
    #[serde(
        rename = "statusType",
        default = "fallback_query_session_debug_result_network_entries_item_status_type"
    )]
    pub status_type: QuerySessionDebugResultNetworkEntriesItemStatusType,
    #[serde(rename = "requestId", default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(
        rename = "providerId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_id: Option<String>,
    #[serde(rename = "modelId", default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(
        rename = "providerKind",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(rename = "baseURL", default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(
        rename = "querySource",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub query_source: Option<String>,
    #[serde(rename = "queryId", default, skip_serializing_if = "Option::is_none")]
    pub query_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<f64>,
    #[serde(
        rename = "maxAttempts",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub max_attempts: Option<f64>,
    #[serde(
        rename = "nextAttempt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub next_attempt: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(
        rename = "statusCode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub status_code: Option<f64>,
    #[serde(
        rename = "durationMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub duration_ms: Option<f64>,
    #[serde(rename = "delayMs", default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<f64>,
    #[serde(rename = "idleMs", default, skip_serializing_if = "Option::is_none")]
    pub idle_ms: Option<f64>,
    #[serde(rename = "timeoutMs", default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(
        rename = "requestHeaders",
        default = "default_query_session_debug_result_network_entries_item_request_headers"
    )]
    pub request_headers: BTreeMap<String, String>,
    #[serde(
        rename = "responseHeaders",
        default = "default_query_session_debug_result_network_entries_item_response_headers"
    )]
    pub response_headers: BTreeMap<String, String>,
    #[serde(rename = "requestHeaderCount")]
    pub request_header_count: f64,
    #[serde(rename = "responseHeaderCount")]
    pub response_header_count: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QuerySessionDebugResultNetworkEntriesItemStatusType {
    ModelRequestStarted,
    ModelRequestCompleted,
    ModelRequestFailed,
    ModelRetryScheduled,
    ModelStreamStalled,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QuerySessionDebugResultNetworkEntriesItemStatusType {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::ModelRequestStarted;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::ModelRequestStarted => "model_request_started",
            Self::ModelRequestCompleted => "model_request_completed",
            Self::ModelRequestFailed => "model_request_failed",
            Self::ModelRetryScheduled => "model_retry_scheduled",
            Self::ModelStreamStalled => "model_stream_stalled",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "model_request_started" => Self::ModelRequestStarted,
            "model_request_completed" => Self::ModelRequestCompleted,
            "model_request_failed" => Self::ModelRequestFailed,
            "model_retry_scheduled" => Self::ModelRetryScheduled,
            "model_stream_stalled" => Self::ModelStreamStalled,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::ModelRequestStarted,
            known => known.clone(),
        }
    }
}

impl Serialize for QuerySessionDebugResultNetworkEntriesItemStatusType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QuerySessionDebugResultNetworkEntriesItemStatusType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionDebugResultCache {
    #[serde(rename = "hitRateRequestCount")]
    pub hit_rate_request_count: f64,
    #[serde(rename = "totalInputTokens")]
    pub total_input_tokens: f64,
    #[serde(rename = "totalCacheReadTokens")]
    pub total_cache_read_tokens: f64,
    #[serde(rename = "hitRate")]
    pub hit_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionFileChangesParams {
    #[serde(rename = "rowId")]
    pub row_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionFileChangesResult {
    pub files: u64,
    pub additions: u64,
    pub deletions: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<RowTurnHeaderFileChangeSummaryState>,
    pub items: Vec<QuerySessionFileChangesResultItemsItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionFileChangesResultItemsItem {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    #[serde(rename = "writeCount")]
    pub write_count: u64,
    #[serde(
        rename = "toolNames",
        default = "default_query_session_file_changes_result_items_item_tool_names"
    )]
    pub tool_names: Vec<String>,
    #[serde(default = "default_query_session_file_changes_result_items_item_patches")]
    pub patches: Vec<QuerySessionFileChangesResultItemsItemPatchesItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionFileChangesResultItemsItemPatchesItem {
    #[serde(rename = "oldStart")]
    pub old_start: f64,
    #[serde(rename = "oldLines")]
    pub old_lines: f64,
    #[serde(rename = "newStart")]
    pub new_start: f64,
    #[serde(rename = "newLines")]
    pub new_lines: f64,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionPlansResult {
    pub plans: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionPromptsParams {
    #[serde(rename = "beforeId", default, skip_serializing_if = "Option::is_none")]
    pub before_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionPromptsResult {
    pub items: Vec<QuerySessionPromptsResultItemsItem>,
    pub before: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionPromptsResultItemsItem {
    #[serde(rename = "rowId")]
    pub row_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    pub text: String,
    #[serde(default = "default_query_session_prompts_result_items_item_answer")]
    pub answer: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionSubagentsParams {
    #[serde(
        rename = "endedCursor",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub ended_cursor: Option<String>,
    #[serde(
        rename = "endedLimit",
        default = "default_query_session_subagents_params_ended_limit"
    )]
    pub ended_limit: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionSubagentsResult {
    #[serde(rename = "childSessionIds")]
    pub child_session_ids: Vec<String>,
    pub running: Vec<QuerySessionSubagentsResultRunningItem>,
    pub ended: QuerySessionSubagentsResultEnded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionSubagentsResultRunningItem {
    #[serde(rename = "childSessionId")]
    pub child_session_id: String,
    #[serde(rename = "agentId", default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(
        rename = "toolCallId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_call_id: Option<String>,
    #[serde(rename = "subagentType")]
    pub subagent_type: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(rename = "startedAt", default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(rename = "endedAt", default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<u64>,
    #[serde(default = "fallback_query_session_subagents_result_running_item_status")]
    pub status: QuerySessionSubagentsResultRunningItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QuerySessionSubagentsResultRunningItemStatus {
    Running,
    Waiting,
    Blocked,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QuerySessionSubagentsResultRunningItemStatus {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Running;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Blocked => "blocked",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "running" => Self::Running,
            "waiting" => Self::Waiting,
            "blocked" => Self::Blocked,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Running,
            known => known.clone(),
        }
    }
}

impl Serialize for QuerySessionSubagentsResultRunningItemStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QuerySessionSubagentsResultRunningItemStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionSubagentsResultEnded {
    pub total: u64,
    pub items: Vec<QuerySessionSubagentsResultEndedItemsItem>,
    #[serde(
        rename = "nextCursor",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionSubagentsResultEndedItemsItem {
    #[serde(rename = "childSessionId")]
    pub child_session_id: String,
    #[serde(rename = "agentId", default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(
        rename = "toolCallId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_call_id: Option<String>,
    #[serde(rename = "subagentType")]
    pub subagent_type: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(rename = "startedAt", default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(rename = "endedAt", default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<u64>,
    #[serde(default = "fallback_query_session_subagents_result_ended_items_item_status")]
    pub status: QuerySessionSubagentsResultEndedItemsItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QuerySessionSubagentsResultEndedItemsItemStatus {
    Success,
    Failed,
    Cancelled,
    Lost,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QuerySessionSubagentsResultEndedItemsItemStatus {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Lost;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Lost => "lost",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "success" => Self::Success,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "lost" => Self::Lost,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Lost,
            known => known.clone(),
        }
    }
}

impl Serialize for QuerySessionSubagentsResultEndedItemsItemStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QuerySessionSubagentsResultEndedItemsItemStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionsFactsParams {
    #[serde(rename = "sessionIds")]
    pub session_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionsFactsResult {
    pub sessions: Vec<QuerySessionsFactsResultSessionsItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionsFactsResultSessionsItem {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub fact: Option<SessionFactFields>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFactFields {
    #[serde(default = "default_session_fact_fields_title")]
    pub title: String,
    #[serde(
        rename = "titleSource",
        default = "default_session_fact_fields_title_source"
    )]
    pub title_source: SessionFactFieldsTitleSource,
    #[serde(
        rename = "createdAt",
        default = "default_session_fact_fields_created_at"
    )]
    pub created_at: f64,
    #[serde(
        rename = "updatedAt",
        default = "default_session_fact_fields_updated_at"
    )]
    pub updated_at: f64,
    #[serde(
        rename = "lastOutcome",
        default = "default_session_fact_fields_last_outcome"
    )]
    pub last_outcome: Option<SessionFactFieldsLastOutcome>,
    #[serde(
        rename = "deletedAt",
        default = "default_session_fact_fields_deleted_at"
    )]
    pub deleted_at: Option<f64>,
    #[serde(
        rename = "parentSessionId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_session_id: Option<String>,
    #[serde(
        rename = "firstInputAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub first_input_at: Option<f64>,
    #[serde(
        rename = "migrationSource",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub migration_source: Option<SessionFactFieldsMigrationSource>,
    #[serde(default = "default_session_fact_fields_config")]
    pub config: SessionFactFieldsConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SessionFactFieldsSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionFactFieldsTitleSource {
    Custom,
    Generated,
    FirstInput,
    Default,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionFactFieldsTitleSource {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Default;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Custom => "custom",
            Self::Generated => "generated",
            Self::FirstInput => "firstInput",
            Self::Default => "default",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "custom" => Self::Custom,
            "generated" => Self::Generated,
            "firstInput" => Self::FirstInput,
            "default" => Self::Default,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Default,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionFactFieldsTitleSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionFactFieldsTitleSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionFactFieldsLastOutcome {
    Success,
    Error,
    Cancelled,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionFactFieldsLastOutcome {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Error;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "success" => Self::Success,
            "error" => Self::Error,
            "cancelled" => Self::Cancelled,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Error,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionFactFieldsLastOutcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionFactFieldsLastOutcome {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionFactFieldsMigrationSource {
    #[serde(rename = "claudeCode")]
    ImportedHistory,
}

impl SessionFactFieldsMigrationSource {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ImportedHistory => "claudeCode",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFactFieldsConfig {
    #[serde(default = "default_session_fact_fields_config_model")]
    pub model: String,
    #[serde(default = "default_session_fact_fields_config_mode")]
    pub mode: String,
    #[serde(default = "default_session_fact_fields_config_thought")]
    pub thought: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFactFieldsSummary {
    #[serde(rename = "lastError", default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(
        rename = "goalTarget",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub goal_target: Option<String>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "changeSummary",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub change_summary: Option<Opaque>,
    #[serde(
        rename = "goalStatus",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub goal_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryToolOutputResult {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryWorkBashOutputParams {
    #[serde(rename = "workId")]
    pub work_id: String,
}
