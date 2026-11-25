//! 排版之前的摘要：da5b5b5e6ab18a51
//! 生成物，不要手改：`session@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults_1::{
    default_row_hook_invocation_created_at_seq, default_row_hook_invocation_executions,
    default_row_hook_invocation_hook_event_name, default_row_hook_invocation_lane,
    default_row_hook_invocation_stable, default_row_hook_invocation_turn_id,
    default_row_reasoning_created_at_seq, default_row_reasoning_duration_ms,
    default_row_reasoning_stable, default_row_reasoning_state, default_row_reasoning_text,
    default_row_reasoning_turn_id, default_row_subagent_created_at_seq,
    default_row_subagent_parent_tool_call_id, default_row_subagent_stable,
    default_row_subagent_status, default_row_subagent_summary_text, default_row_subagent_turn_id,
    default_row_timeline_marker_created_at_seq, default_row_timeline_marker_marker,
    default_row_timeline_marker_stable, default_row_timeline_marker_turn_id,
    default_row_tool_call_backgrounded, default_row_tool_call_created_at_seq,
    default_row_tool_call_input_text, default_row_tool_call_stable, default_row_tool_call_status,
    default_row_tool_call_tool_name, default_row_tool_call_turn_id,
};
use super::{
    CompactMarkerDetail, QuerySessionDebugResultCache, QuerySessionDebugResultNetworkEntriesItem,
    RowTurnHeaderState,
};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowAssistantTextFeedback {
    None,
    Up,
    Down,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl RowAssistantTextFeedback {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::None;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Up => "up",
            Self::Down => "down",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "none" => Self::None,
            "up" => Self::Up,
            "down" => Self::Down,
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

impl Serialize for RowAssistantTextFeedback {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RowAssistantTextFeedback {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowReasoning {
    pub id: String,
    pub v: u64,
    #[serde(rename = "turnId", default = "default_row_reasoning_turn_id")]
    pub turn_id: String,
    #[serde(
        rename = "createdAtSeq",
        default = "default_row_reasoning_created_at_seq"
    )]
    pub created_at_seq: u64,
    #[serde(default = "default_row_reasoning_stable")]
    pub stable: bool,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
    #[serde(default = "default_row_reasoning_text")]
    pub text: String,
    #[serde(default = "default_row_reasoning_state")]
    pub state: RowReasoningState,
    #[serde(rename = "durationMs", default = "default_row_reasoning_duration_ms")]
    pub duration_ms: f64,
    #[serde(
        rename = "assistantResponseId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub assistant_response_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowReasoningState {
    Streaming,
    Done,
    Interrupted,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl RowReasoningState {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Done;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Streaming => "streaming",
            Self::Done => "done",
            Self::Interrupted => "interrupted",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "streaming" => Self::Streaming,
            "done" => Self::Done,
            "interrupted" => Self::Interrupted,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Done,
            known => known.clone(),
        }
    }
}

impl Serialize for RowReasoningState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RowReasoningState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowToolCall {
    pub id: String,
    pub v: u64,
    #[serde(rename = "turnId", default = "default_row_tool_call_turn_id")]
    pub turn_id: String,
    #[serde(
        rename = "createdAtSeq",
        default = "default_row_tool_call_created_at_seq"
    )]
    pub created_at_seq: u64,
    #[serde(default = "default_row_tool_call_stable")]
    pub stable: bool,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
    #[serde(default = "default_row_tool_call_status")]
    pub status: RowToolCallStatus,
    #[serde(rename = "toolName", default = "default_row_tool_call_tool_name")]
    pub tool_name: String,
    #[serde(rename = "inputText", default = "default_row_tool_call_input_text")]
    pub input_text: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Opaque>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Opaque>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Opaque>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "outputPreview",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_preview: Option<Opaque>,
    #[serde(
        rename = "approvalInteractionId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub approval_interaction_id: Option<String>,
    #[serde(default = "default_row_tool_call_backgrounded")]
    pub backgrounded: bool,
    #[serde(rename = "workId", default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<String>,
    #[serde(
        rename = "assistantResponseId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub assistant_response_id: Option<String>,
    #[serde(rename = "errorCode", default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(rename = "cuaApp", default, skip_serializing_if = "Option::is_none")]
    pub cua_app: Option<Opaque>,
    #[serde(rename = "startedAt", default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    #[serde(rename = "endedAt", default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowToolCallStatus {
    Pending,
    Approved,
    Running,
    Completed,
    Error,
    Denied,
    InputStreaming,
    PendingApproval,
    Cancelled,
    Unknown,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl RowToolCallStatus {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unknown;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Denied => "denied",
            Self::InputStreaming => "inputStreaming",
            Self::PendingApproval => "pendingApproval",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "pending" => Self::Pending,
            "approved" => Self::Approved,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "error" => Self::Error,
            "denied" => Self::Denied,
            "inputStreaming" => Self::InputStreaming,
            "pendingApproval" => Self::PendingApproval,
            "cancelled" => Self::Cancelled,
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

impl Serialize for RowToolCallStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RowToolCallStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowSubagent {
    pub id: String,
    pub v: u64,
    #[serde(rename = "turnId", default = "default_row_subagent_turn_id")]
    pub turn_id: String,
    #[serde(
        rename = "createdAtSeq",
        default = "default_row_subagent_created_at_seq"
    )]
    pub created_at_seq: u64,
    #[serde(default = "default_row_subagent_stable")]
    pub stable: bool,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
    #[serde(
        rename = "parentToolCallId",
        default = "default_row_subagent_parent_tool_call_id"
    )]
    pub parent_tool_call_id: String,
    #[serde(default = "default_row_subagent_status")]
    pub status: RowTurnHeaderState,
    #[serde(rename = "summaryText", default = "default_row_subagent_summary_text")]
    pub summary_text: String,
    #[serde(
        rename = "childSessionId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub child_session_id: Option<String>,
    #[serde(
        rename = "subagentType",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub subagent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backgrounded: Option<bool>,
    #[serde(rename = "workId", default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<String>,
    #[serde(rename = "startedAt", default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    #[serde(rename = "endedAt", default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowHookInvocation {
    pub id: String,
    pub v: u64,
    #[serde(rename = "turnId", default = "default_row_hook_invocation_turn_id")]
    pub turn_id: String,
    #[serde(
        rename = "createdAtSeq",
        default = "default_row_hook_invocation_created_at_seq"
    )]
    pub created_at_seq: u64,
    #[serde(default = "default_row_hook_invocation_stable")]
    pub stable: bool,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
    #[serde(
        rename = "hookEventName",
        default = "default_row_hook_invocation_hook_event_name"
    )]
    pub hook_event_name: String,
    #[serde(default = "default_row_hook_invocation_lane")]
    pub lane: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default = "default_row_hook_invocation_executions")]
    pub executions: Vec<Opaque>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<RowHookInvocationState>,
    #[serde(rename = "hookCount", default, skip_serializing_if = "Option::is_none")]
    pub hook_count: Option<u64>,
    #[serde(rename = "startedAt", default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    #[serde(rename = "endedAt", default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<f64>,
    #[serde(
        rename = "durationMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub duration_ms: Option<f64>,
    #[serde(
        rename = "anchorToolCallId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub anchor_tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowHookInvocationState {
    Running,
    Completed,
    Failed,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl RowHookInvocationState {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Completed;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Completed,
            known => known.clone(),
        }
    }
}

impl Serialize for RowHookInvocationState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RowHookInvocationState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowTimelineMarker {
    pub id: String,
    pub v: u64,
    #[serde(rename = "turnId", default = "default_row_timeline_marker_turn_id")]
    pub turn_id: String,
    #[serde(
        rename = "createdAtSeq",
        default = "default_row_timeline_marker_created_at_seq"
    )]
    pub created_at_seq: u64,
    #[serde(default = "default_row_timeline_marker_stable")]
    pub stable: bool,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
    #[serde(default = "default_row_timeline_marker_marker")]
    pub marker: RowTimelineMarkerMarker,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<CompactMarkerDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<RowTimelineMarkerLane>,
    #[serde(
        rename = "sourceCommandId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowTimelineMarkerMarker {
    Compact,
    Fork,
    ModelChange,
    Goal,
    Retry,
    Checkpoint,
    Unknown,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl RowTimelineMarkerMarker {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unknown;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Compact => "compact",
            Self::Fork => "fork",
            Self::ModelChange => "modelChange",
            Self::Goal => "goal",
            Self::Retry => "retry",
            Self::Checkpoint => "checkpoint",
            Self::Unknown => "unknown",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "compact" => Self::Compact,
            "fork" => Self::Fork,
            "modelChange" => Self::ModelChange,
            "goal" => Self::Goal,
            "retry" => Self::Retry,
            "checkpoint" => Self::Checkpoint,
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

impl Serialize for RowTimelineMarkerMarker {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RowTimelineMarkerMarker {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowTimelineMarkerLane {
    AssistantWork,
    TurnTailBoundary,
    LightBoundary,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl RowTimelineMarkerLane {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::AssistantWork;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::AssistantWork => "assistantWork",
            Self::TurnTailBoundary => "turnTailBoundary",
            Self::LightBoundary => "lightBoundary",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "assistantWork" => Self::AssistantWork,
            "turnTailBoundary" => Self::TurnTailBoundary,
            "lightBoundary" => Self::LightBoundary,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::AssistantWork,
            known => known.clone(),
        }
    }
}

impl Serialize for RowTimelineMarkerLane {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RowTimelineMarkerLane {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionDebugParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionDebugResult {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub rounds: Vec<QuerySessionDebugResultRoundsItem>,
    #[serde(rename = "networkEntries")]
    pub network_entries: Vec<QuerySessionDebugResultNetworkEntriesItem>,
    pub cache: Option<QuerySessionDebugResultCache>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionDebugResultRoundsItem {
    #[serde(rename = "eventKey")]
    pub event_key: String,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "requestIndex")]
    pub request_index: f64,
    #[serde(rename = "recordedAt")]
    pub recorded_at: f64,
    pub usage: QuerySessionDebugResultRoundsItemUsage,
    #[serde(rename = "hitRate")]
    pub hit_rate: Option<f64>,
    #[serde(rename = "generationDurationMs")]
    pub generation_duration_ms: Option<f64>,
    #[serde(rename = "tokensPerSecond")]
    pub tokens_per_second: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionDebugResultRoundsItemUsage {
    #[serde(
        rename = "inputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_tokens: Option<f64>,
    #[serde(
        rename = "outputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_tokens: Option<f64>,
    #[serde(
        rename = "totalTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub total_tokens: Option<f64>,
    #[serde(
        rename = "reasoningTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_tokens: Option<f64>,
    #[serde(
        rename = "cachedInputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cached_input_tokens: Option<f64>,
    #[serde(
        rename = "cachedWriteInputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cached_write_input_tokens: Option<f64>,
}
