//! 排版之前的摘要：4b43e092f4879218
//! 生成物，不要手改：`session@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults_1::{
    default_interaction_mcp_sampling_payload_messages, default_query_rows_range_params_limit,
    default_row_assistant_text_created_at_seq, default_row_assistant_text_feedback,
    default_row_assistant_text_model, default_row_assistant_text_stable,
    default_row_assistant_text_state, default_row_assistant_text_text,
    default_row_assistant_text_turn_id, default_row_turn_header_created_at_seq,
    default_row_turn_header_durations, default_row_turn_header_durations_active_ms,
    default_row_turn_header_durations_total_ms, default_row_turn_header_execution_kind,
    default_row_turn_header_file_change_summary_additions,
    default_row_turn_header_file_change_summary_deletions,
    default_row_turn_header_file_change_summary_files,
    default_row_turn_header_file_change_summary_state, default_row_turn_header_file_changes,
    default_row_turn_header_origin, default_row_turn_header_output_tokens,
    default_row_turn_header_stable, default_row_turn_header_state,
    default_row_turn_header_timing_model_duration_ms,
    default_row_turn_header_timing_other_duration_ms,
    default_row_turn_header_timing_tool_duration_ms, default_row_turn_header_turn_id,
    default_row_turn_header_work_segments, default_row_user_input_attachments,
    default_row_user_input_created_at_seq, default_row_user_input_guided,
    default_row_user_input_origin, default_row_user_input_stable, default_row_user_input_text,
    default_row_user_input_turn_id, fallback_sampling_turn_role,
};
use super::{
    HighspeedRow, InputQuote, InputSource, InteractionOrigin, RowAssistantTextFeedback,
    RowHookInvocation, RowReasoning, RowSubagent, RowTimelineMarker, RowToolCall,
};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InteractionMcpElicitationPayloadMode {
    Form,
    Url,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl InteractionMcpElicitationPayloadMode {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Form;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Form => "form",
            Self::Url => "url",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "form" => Self::Form,
            "url" => Self::Url,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Form,
            known => known.clone(),
        }
    }
}

impl Serialize for InteractionMcpElicitationPayloadMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InteractionMcpElicitationPayloadMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionMcpSampling {
    pub id: String,
    pub v: u64,
    pub payload: InteractionMcpSamplingPayload,
    #[serde(rename = "expiresAt", default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionMcpSamplingPayload {
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    #[serde(rename = "serverName")]
    pub server_name: String,
    #[serde(default = "default_interaction_mcp_sampling_payload_messages")]
    pub messages: Vec<SamplingTurn>,
    #[serde(
        rename = "systemPrompt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub system_prompt: Option<String>,
    #[serde(rename = "maxTokens")]
    pub max_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<InteractionOrigin>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SamplingTurn {
    #[serde(default = "fallback_sampling_turn_role")]
    pub role: SamplingTurnRole,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SamplingTurnRole {
    User,
    Assistant,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SamplingTurnRole {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::User;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::User,
            known => known.clone(),
        }
    }
}

impl Serialize for SamplingTurnRole {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SamplingTurnRole {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryRowsRangeParams {
    #[serde(rename = "beforeId", default, skip_serializing_if = "Option::is_none")]
    pub before_id: Option<String>,
    #[serde(default = "default_query_rows_range_params_limit")]
    pub limit: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryRowsRangeResult {
    pub rows: Vec<Row>,
    #[serde(rename = "hasMore")]
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Row {
    #[serde(rename = "turnHeader")]
    TurnHeader(RowTurnHeader),
    #[serde(rename = "userInput")]
    UserInput(RowUserInput),
    #[serde(rename = "assistantText")]
    AssistantText(RowAssistantText),
    #[serde(rename = "reasoning")]
    Reasoning(RowReasoning),
    #[serde(rename = "toolCall")]
    ToolCall(RowToolCall),
    #[serde(rename = "subagent")]
    Subagent(RowSubagent),
    #[serde(rename = "hookInvocation")]
    HookInvocation(RowHookInvocation),
    #[serde(rename = "timelineMarker")]
    TimelineMarker(RowTimelineMarker),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowTurnHeader {
    pub id: String,
    pub v: u64,
    #[serde(rename = "turnId", default = "default_row_turn_header_turn_id")]
    pub turn_id: String,
    #[serde(
        rename = "createdAtSeq",
        default = "default_row_turn_header_created_at_seq"
    )]
    pub created_at_seq: u64,
    #[serde(default = "default_row_turn_header_stable")]
    pub stable: bool,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
    #[serde(default = "default_row_turn_header_origin")]
    pub origin: RowTurnHeaderOrigin,
    #[serde(
        rename = "executionKind",
        default = "default_row_turn_header_execution_kind"
    )]
    pub execution_kind: String,
    #[serde(default = "default_row_turn_header_state")]
    pub state: RowTurnHeaderState,
    #[serde(default = "default_row_turn_header_durations")]
    pub durations: RowTurnHeaderDurations,
    #[serde(
        rename = "outputTokens",
        default = "default_row_turn_header_output_tokens"
    )]
    pub output_tokens: f64,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "workSegments",
        default = "default_row_turn_header_work_segments"
    )]
    pub work_segments: Vec<Opaque>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "fileChanges",
        default = "default_row_turn_header_file_changes"
    )]
    pub file_changes: Vec<Opaque>,
    #[serde(
        rename = "fileChangeSummary",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub file_change_summary: Option<RowTurnHeaderFileChangeSummary>,
    #[serde(
        rename = "sourceCommandId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_command_id: Option<String>,
    #[serde(
        rename = "historyRoundCount",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub history_round_count: Option<u64>,
    #[serde(rename = "startedAt", default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    #[serde(rename = "endedAt", default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<RowTurnHeaderTiming>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "originMeta",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub origin_meta: Option<Opaque>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "workflowLaunch",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub workflow_launch: Option<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowTurnHeaderOrigin {
    User,
    Queue,
    Automation,
    OffPeak,
    Bot,
    BackgroundResult,
    GoalContinuation,
    EditRerun,
    WorkflowLaunch,
    Mailbox,
    Synthetic,
    Unknown,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl RowTurnHeaderOrigin {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unknown;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::User => "user",
            Self::Queue => "queue",
            Self::Automation => "automation",
            Self::OffPeak => "offPeak",
            Self::Bot => "bot",
            Self::BackgroundResult => "backgroundResult",
            Self::GoalContinuation => "goalContinuation",
            Self::EditRerun => "editRerun",
            Self::WorkflowLaunch => "workflowLaunch",
            Self::Mailbox => "mailbox",
            Self::Synthetic => "synthetic",
            Self::Unknown => "unknown",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "user" => Self::User,
            "queue" => Self::Queue,
            "automation" => Self::Automation,
            "offPeak" => Self::OffPeak,
            "bot" => Self::Bot,
            "backgroundResult" => Self::BackgroundResult,
            "goalContinuation" => Self::GoalContinuation,
            "editRerun" => Self::EditRerun,
            "workflowLaunch" => Self::WorkflowLaunch,
            "mailbox" => Self::Mailbox,
            "synthetic" => Self::Synthetic,
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

impl Serialize for RowTurnHeaderOrigin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RowTurnHeaderOrigin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowTurnHeaderState {
    Running,
    Completed,
    Error,
    Cancelled,
    Unknown,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl RowTurnHeaderState {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unknown;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "running" => Self::Running,
            "completed" => Self::Completed,
            "error" => Self::Error,
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

impl Serialize for RowTurnHeaderState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RowTurnHeaderState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowTurnHeaderDurations {
    #[serde(
        rename = "activeMs",
        default = "default_row_turn_header_durations_active_ms"
    )]
    pub active_ms: f64,
    #[serde(
        rename = "totalMs",
        default = "default_row_turn_header_durations_total_ms"
    )]
    pub total_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowTurnHeaderFileChangeSummary {
    #[serde(default = "default_row_turn_header_file_change_summary_additions")]
    pub additions: f64,
    #[serde(default = "default_row_turn_header_file_change_summary_deletions")]
    pub deletions: f64,
    #[serde(default = "default_row_turn_header_file_change_summary_files")]
    pub files: f64,
    #[serde(default = "default_row_turn_header_file_change_summary_state")]
    pub state: RowTurnHeaderFileChangeSummaryState,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowTurnHeaderFileChangeSummaryState {
    Active,
    Reverted,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl RowTurnHeaderFileChangeSummaryState {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Active;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Active => "active",
            Self::Reverted => "reverted",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "active" => Self::Active,
            "reverted" => Self::Reverted,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Active,
            known => known.clone(),
        }
    }
}

impl Serialize for RowTurnHeaderFileChangeSummaryState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RowTurnHeaderFileChangeSummaryState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowTurnHeaderTiming {
    #[serde(
        rename = "modelDurationMs",
        default = "default_row_turn_header_timing_model_duration_ms"
    )]
    pub model_duration_ms: f64,
    #[serde(
        rename = "toolDurationMs",
        default = "default_row_turn_header_timing_tool_duration_ms"
    )]
    pub tool_duration_ms: f64,
    #[serde(
        rename = "otherDurationMs",
        default = "default_row_turn_header_timing_other_duration_ms"
    )]
    pub other_duration_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowUserInput {
    pub id: String,
    pub v: u64,
    #[serde(rename = "turnId", default = "default_row_user_input_turn_id")]
    pub turn_id: String,
    #[serde(
        rename = "createdAtSeq",
        default = "default_row_user_input_created_at_seq"
    )]
    pub created_at_seq: u64,
    #[serde(default = "default_row_user_input_stable")]
    pub stable: bool,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
    #[serde(default = "default_row_user_input_text")]
    pub text: String,
    #[serde(default = "default_row_user_input_origin")]
    pub origin: RowTurnHeaderOrigin,
    #[serde(default = "default_row_user_input_guided")]
    pub guided: bool,
    #[serde(
        rename = "sourceCommandId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_command_id: Option<String>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default = "default_row_user_input_attachments")]
    pub attachments: Vec<Opaque>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highspeed: Option<HighspeedRow>,
    #[serde(
        rename = "epilogueStart",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub epilogue_start: Option<u64>,
    #[serde(
        rename = "rootSourceCommandId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub root_source_command_id: Option<String>,
    #[serde(rename = "clientId", default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "originMeta",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub origin_meta: Option<Opaque>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "workflowLaunch",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub workflow_launch: Option<Opaque>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quotes: Option<Vec<InputQuote>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<InputSource>,
    #[serde(
        rename = "modelSelection",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_selection: Option<RowUserInputModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(
        rename = "planEnabled",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowUserInputModelSelection {
    #[serde(rename = "providerId")]
    pub provider_id: String,
    #[serde(rename = "modelId")]
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<RowUserInputModelSelectionOptions>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowUserInputModelSelectionOptions {
    #[serde(
        rename = "reasoningLevel",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_level: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowAssistantText {
    pub id: String,
    pub v: u64,
    #[serde(rename = "turnId", default = "default_row_assistant_text_turn_id")]
    pub turn_id: String,
    #[serde(
        rename = "createdAtSeq",
        default = "default_row_assistant_text_created_at_seq"
    )]
    pub created_at_seq: u64,
    #[serde(default = "default_row_assistant_text_stable")]
    pub stable: bool,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
    #[serde(default = "default_row_assistant_text_text")]
    pub text: String,
    #[serde(default = "default_row_assistant_text_state")]
    pub state: RowAssistantTextState,
    #[serde(default = "default_row_assistant_text_model")]
    pub model: String,
    #[serde(default = "default_row_assistant_text_feedback")]
    pub feedback: RowAssistantTextFeedback,
    #[serde(
        rename = "assistantResponseId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub assistant_response_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowAssistantTextState {
    Streaming,
    Done,
    Error,
    Interrupted,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl RowAssistantTextState {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Done;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Streaming => "streaming",
            Self::Done => "done",
            Self::Error => "error",
            Self::Interrupted => "interrupted",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "streaming" => Self::Streaming,
            "done" => Self::Done,
            "error" => Self::Error,
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

impl Serialize for RowAssistantTextState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RowAssistantTextState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}
