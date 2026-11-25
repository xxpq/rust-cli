//! 排版之前的摘要：c18aaf1bd37ef274
//! 生成物，不要手改：`session@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults_1::{
    default_command_input_send_attachments, default_command_input_send_context_refs,
    default_command_result_turn_stop_cancelled, default_command_result_turn_stop_still_queued,
    default_command_session_create_execution_directories, default_command_session_create_kind,
    fallback_command_result_input_send_delivery,
};
use super::{CompactMarkerDetailOrigin, CompactMarkerDetailStatus};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskedQuestionOption {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskedQuestion {
    pub question: String,
    pub header: String,
    pub options: Vec<AskedQuestionOption>,
    #[serde(
        rename = "multiSelect",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub multi_select: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandConfigSetFollowupMode {
    #[serde(rename = "followupMode")]
    pub followup_mode: CommandConfigSetFollowupModeFollowupMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandConfigSetFollowupModeFollowupMode {
    #[serde(rename = "queue")]
    Queue,
    #[serde(rename = "guide")]
    Guide,
}

impl CommandConfigSetFollowupModeFollowupMode {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Guide => "guide",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandConfigSetMode {
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandConfigSetModel {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandContextDiscardShared {
    #[serde(rename = "contextId")]
    pub context_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputSend {
    pub text: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default = "default_command_input_send_attachments")]
    pub attachments: Vec<Opaque>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<CommandInputSendDisposition>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Opaque>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "contextRefs",
        default = "default_command_input_send_context_refs"
    )]
    pub context_refs: Vec<Opaque>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation: Option<Opaque>,
    #[serde(
        rename = "modelSelection",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_selection: Option<CommandInputSendModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(
        rename = "planEnabled",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_enabled: Option<bool>,
    #[serde(
        rename = "modelExecution",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_execution: Option<CommandInputSendModelExecution>,
    #[serde(
        rename = "highspeedMeta",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub highspeed_meta: Option<CommandInputSendHighspeedMeta>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "browserAmbientContext",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub browser_ambient_context: Option<Opaque>,
    #[serde(
        rename = "automationId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub automation_id: Option<String>,
    #[serde(
        rename = "offPeakTaskId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub off_peak_task_id: Option<String>,
    #[serde(
        rename = "offPeakRunType",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub off_peak_run_type: Option<CommandInputSendOffPeakRunType>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "botDeliveryTarget",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub bot_delivery_target: Option<Opaque>,
    #[serde(
        rename = "toolDisallowlist",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_disallowlist: Option<Vec<String>>,
    #[serde(rename = "turnId", default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(
        rename = "expectedHeldQueueItemIds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub expected_held_queue_item_ids: Option<Vec<String>>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft: Option<Opaque>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quotes: Option<Vec<CommandInputSendQuotesItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CommandInputSendSource>,
    #[serde(
        rename = "genUiContext",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub gen_ui_context: Option<Vec<CommandInputSendGenUiContextItem>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandInputSendDisposition {
    #[serde(rename = "startNow")]
    StartNow,
    #[serde(rename = "enqueue")]
    Enqueue,
    #[serde(rename = "guide")]
    Guide,
    #[serde(rename = "keepQueue")]
    KeepQueue,
    #[serde(rename = "discardQueue")]
    DiscardQueue,
}

impl CommandInputSendDisposition {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StartNow => "startNow",
            Self::Enqueue => "enqueue",
            Self::Guide => "guide",
            Self::KeepQueue => "keepQueue",
            Self::DiscardQueue => "discardQueue",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputSendModelSelection {
    #[serde(rename = "providerId")]
    pub provider_id: String,
    #[serde(rename = "modelId")]
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<CommandInputSendModelSelectionOptions>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputSendModelSelectionOptions {
    #[serde(
        rename = "reasoningLevel",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_level: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputSendModelExecution {
    pub selection: CommandInputSendModelSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CommandInputSendModelExecutionCredential>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<CommandInputSendModelExecutionFallback>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CommandInputSendModelExecutionCredential {
    #[serde(rename = "highspeedCard")]
    HighspeedCard(CommandInputSendModelExecutionCredentialHighspeedCard),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputSendModelExecutionCredentialHighspeedCard {
    #[serde(rename = "cardId")]
    pub card_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputSendModelExecutionFallback {
    pub rules: Vec<CommandInputSendModelExecutionFallbackRulesItem>,
    pub unavailable: CommandInputSendModelExecutionFallbackRulesItemReason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputSendModelExecutionFallbackRulesItem {
    pub reason: CommandInputSendModelExecutionFallbackRulesItemReason,
    #[serde(
        rename = "providerErrorCode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_error_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandInputSendModelExecutionFallbackRulesItemReason {
    #[serde(rename = "highspeed_card_expired")]
    HighspeedCardExpired,
    #[serde(rename = "highspeed_request_failed")]
    HighspeedRequestFailed,
    #[serde(rename = "highspeed_selection_unavailable")]
    HighspeedSelectionUnavailable,
}

impl CommandInputSendModelExecutionFallbackRulesItemReason {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HighspeedCardExpired => "highspeed_card_expired",
            Self::HighspeedRequestFailed => "highspeed_request_failed",
            Self::HighspeedSelectionUnavailable => "highspeed_selection_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputSendHighspeedMeta {
    #[serde(rename = "schemaVersion")]
    pub schema_version: i64,
    #[serde(rename = "cardId")]
    pub card_id: String,
    #[serde(rename = "taskId")]
    pub task_id: String,
    pub provider: String,
    pub model: String,
    #[serde(rename = "issuedAt")]
    pub issued_at: u64,
    #[serde(rename = "expiresAt")]
    pub expires_at: u64,
    #[serde(
        rename = "regularTps",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub regular_tps: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandInputSendOffPeakRunType {
    #[serde(rename = "init")]
    Init,
    #[serde(rename = "resume")]
    Resume,
}

impl CommandInputSendOffPeakRunType {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Resume => "resume",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputSendQuotesItem {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(
        rename = "senderName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sender_name: Option<String>,
    #[serde(rename = "senderId", default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    #[serde(rename = "messageId", default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(rename = "sentAt", default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CommandInputSendSource {
    #[serde(rename = "pluginUi")]
    PluginUi(CommandInputSendSourcePluginUi),
    #[serde(rename = "genUi")]
    GenUi(CommandInputSendSourceGenUi),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputSendSourcePluginUi {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputSendSourceGenUi {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputSendGenUiContextItem {
    pub path: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub state: Opaque,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInteractionResolve {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub answer: Opaque,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInteractionSnooze {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRowReportHighspeed {
    pub metrics: CommandRowReportHighspeedMetrics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRowReportHighspeedMetrics {
    #[serde(rename = "regularTps")]
    pub regular_tps: f64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "durationMs")]
    pub duration_ms: f64,
    #[serde(rename = "highspeedTps")]
    pub highspeed_tps: f64,
    #[serde(rename = "savedDurationMs")]
    pub saved_duration_ms: f64,
    #[serde(
        rename = "modelDurationMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_duration_ms: Option<f64>,
    #[serde(
        rename = "toolDurationMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_duration_ms: Option<f64>,
    #[serde(
        rename = "otherDurationMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub other_duration_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSessionCompact {
    #[serde(
        rename = "modelSelection",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_selection: Option<CommandInputSendModelSelection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSessionCreate {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(default = "default_command_session_create_kind")]
    pub kind: CommandSessionCreateKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "firstInput",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub first_input: Option<Opaque>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Opaque>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "importedHistory",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub imported_history: Option<Opaque>,
    #[serde(
        rename = "executionDirectories",
        default = "default_command_session_create_execution_directories"
    )]
    pub execution_directories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<CommandSessionCreateOwnership>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistence: Option<CommandSessionCreatePersistence>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "mcpServers",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub mcp_servers: Option<Vec<Opaque>>,
    #[serde(
        rename = "toolAllowlist",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_allowlist: Option<Vec<String>>,
    #[serde(
        rename = "toolDenylist",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_denylist: Option<Vec<String>>,
    #[serde(
        rename = "titleGenerationEnabled",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub title_generation_enabled: Option<bool>,
    #[serde(
        rename = "offPeakToolEnabled",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub off_peak_tool_enabled: Option<bool>,
    #[serde(
        rename = "dynamicWorkflowEnabled",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub dynamic_workflow_enabled: Option<bool>,
    #[serde(
        rename = "dynamicWorkflowMode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub dynamic_workflow_mode: Option<CommandSessionCreateDynamicWorkflowMode>,
    #[serde(rename = "taskType", default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandSessionCreateKind {
    #[serde(rename = "main")]
    Main,
    #[serde(rename = "sideChat")]
    SideChat,
    #[serde(rename = "child")]
    Child,
}

impl CommandSessionCreateKind {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::SideChat => "sideChat",
            Self::Child => "child",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSessionCreateOwnership {
    #[serde(rename = "projectId")]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandSessionCreatePersistence {
    #[serde(rename = "immediate")]
    Immediate,
    #[serde(rename = "deferred")]
    Deferred,
}

impl CommandSessionCreatePersistence {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Deferred => "deferred",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandSessionCreateDynamicWorkflowMode {
    #[serde(rename = "disabled")]
    Disabled,
    #[serde(rename = "onDemand")]
    OnDemand,
    #[serde(rename = "alwaysOn")]
    AlwaysOn,
}

impl CommandSessionCreateDynamicWorkflowMode {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::OnDemand => "onDemand",
            Self::AlwaysOn => "alwaysOn",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSessionRename {
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandTurnStop {
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultInputSend {
    #[serde(default = "fallback_command_result_input_send_delivery")]
    pub delivery: CommandResultInputSendDelivery,
    #[serde(rename = "inputId")]
    pub input_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CommandResultInputSendDelivery {
    StartNow,
    Queue,
    Guide,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl CommandResultInputSendDelivery {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Queue;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::StartNow => "startNow",
            Self::Queue => "queue",
            Self::Guide => "guide",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "startNow" => Self::StartNow,
            "queue" => Self::Queue,
            "guide" => Self::Guide,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Queue,
            known => known.clone(),
        }
    }
}

impl Serialize for CommandResultInputSendDelivery {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CommandResultInputSendDelivery {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultSessionCreate {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultTurnStop {
    #[serde(
        rename = "stillQueued",
        default = "default_command_result_turn_stop_still_queued"
    )]
    pub still_queued: Vec<String>,
    #[serde(default = "default_command_result_turn_stop_cancelled")]
    pub cancelled: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactMarkerDetail {
    pub r#type: CompactMarkerDetailType,
    pub origin: CompactMarkerDetailOrigin,
    pub status: CompactMarkerDetailStatus,
    #[serde(
        rename = "tokensBefore",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tokens_before: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompactMarkerDetailType {
    #[serde(rename = "compact")]
    Compact,
}

impl CompactMarkerDetailType {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
        }
    }
}
