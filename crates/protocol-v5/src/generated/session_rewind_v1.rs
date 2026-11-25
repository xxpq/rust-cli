//! 排版之前的摘要：403b1f11f64ec252
//! 生成物，不要手改：`session.rewind@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandFilesRewind {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandTurnEdit {
    #[serde(rename = "newText")]
    pub new_text: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Opaque>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quotes: Option<Vec<CommandTurnEditQuotesItem>>,
    #[serde(
        rename = "modelSelection",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_selection: Option<CommandTurnEditModelSelection>,
    #[serde(
        rename = "modelExecution",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_execution: Option<CommandTurnEditModelExecution>,
    #[serde(
        rename = "highspeedMeta",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub highspeed_meta: Option<CommandTurnEditHighspeedMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(
        rename = "planEnabled",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_enabled: Option<bool>,
    #[serde(
        rename = "workspaceMode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_mode: Option<CommandTurnEditWorkspaceMode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandTurnEditQuotesItem {
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
#[serde(deny_unknown_fields)]
pub struct CommandTurnEditModelSelection {
    #[serde(rename = "providerId")]
    pub provider_id: String,
    #[serde(rename = "modelId")]
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<CommandTurnEditModelSelectionOptions>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandTurnEditModelSelectionOptions {
    #[serde(
        rename = "reasoningLevel",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_level: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandTurnEditModelExecution {
    pub selection: CommandTurnEditModelSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CommandTurnEditModelExecutionCredential>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<CommandTurnEditModelExecutionFallback>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CommandTurnEditModelExecutionCredential {
    #[serde(rename = "highspeedCard")]
    HighspeedCard(CommandTurnEditModelExecutionCredentialHighspeedCard),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandTurnEditModelExecutionCredentialHighspeedCard {
    #[serde(rename = "cardId")]
    pub card_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandTurnEditModelExecutionFallback {
    pub rules: Vec<CommandTurnEditModelExecutionFallbackRulesItem>,
    pub unavailable: CommandTurnEditModelExecutionFallbackRulesItemReason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandTurnEditModelExecutionFallbackRulesItem {
    pub reason: CommandTurnEditModelExecutionFallbackRulesItemReason,
    #[serde(
        rename = "providerErrorCode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_error_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandTurnEditModelExecutionFallbackRulesItemReason {
    #[serde(rename = "highspeed_card_expired")]
    HighspeedCardExpired,
    #[serde(rename = "highspeed_request_failed")]
    HighspeedRequestFailed,
    #[serde(rename = "highspeed_selection_unavailable")]
    HighspeedSelectionUnavailable,
}

impl CommandTurnEditModelExecutionFallbackRulesItemReason {
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
pub struct CommandTurnEditHighspeedMeta {
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
pub enum CommandTurnEditWorkspaceMode {
    #[serde(rename = "preserve")]
    Preserve,
    #[serde(rename = "rewind")]
    Rewind,
}

impl CommandTurnEditWorkspaceMode {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preserve => "preserve",
            Self::Rewind => "rewind",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultTurnEdit {
    #[serde(default = "fallback_command_result_turn_edit_disposition")]
    pub disposition: CommandResultTurnEditDisposition,
    #[serde(
        rename = "reasonCode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reason_code: Option<String>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CommandResultTurnEditDisposition {
    Rewind,
    Blocked,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl CommandResultTurnEditDisposition {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Blocked;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Rewind => "rewind",
            Self::Blocked => "blocked",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "rewind" => Self::Rewind,
            "blocked" => Self::Blocked,
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

impl Serialize for CommandResultTurnEditDisposition {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CommandResultTurnEditDisposition {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionFileRewindPreviewParams {
    #[serde(rename = "rowId")]
    pub row_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionFileRewindPreviewResult {
    #[serde(rename = "canApply")]
    pub can_apply: bool,
    #[serde(rename = "ignoredFiles")]
    pub ignored_files: Vec<QuerySessionFileRewindPreviewResultIgnoredFilesItem>,
    #[serde(rename = "safeFiles")]
    pub safe_files: Vec<QuerySessionFileRewindPreviewResultSafeFilesItem>,
    #[serde(rename = "unsafeFiles")]
    pub unsafe_files: Vec<QuerySessionFileRewindPreviewResultUnsafeFilesItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionFileRewindPreviewResultIgnoredFilesItem {
    #[serde(rename = "operationCount")]
    pub operation_count: u64,
    pub path: String,
    pub reason: QuerySessionFileRewindPreviewResultIgnoredFilesItemReason,
    #[serde(
        rename = "toolNames",
        default = "default_query_session_file_rewind_preview_result_ignored_files_item_tool_names"
    )]
    pub tool_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QuerySessionFileRewindPreviewResultIgnoredFilesItemReason {
    #[serde(rename = "bash_ignored")]
    BashIgnored,
}

impl QuerySessionFileRewindPreviewResultIgnoredFilesItemReason {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BashIgnored => "bash_ignored",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionFileRewindPreviewResultSafeFilesItem {
    #[serde(default = "fallback_query_session_file_rewind_preview_result_safe_files_item_action")]
    pub action: QuerySessionFileRewindPreviewResultSafeFilesItemAction,
    #[serde(rename = "operationCount")]
    pub operation_count: u64,
    pub path: String,
    #[serde(
        rename = "toolNames",
        default = "default_query_session_file_rewind_preview_result_safe_files_item_tool_names"
    )]
    pub tool_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QuerySessionFileRewindPreviewResultSafeFilesItemAction {
    Restore,
    Delete,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QuerySessionFileRewindPreviewResultSafeFilesItemAction {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Restore;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Restore => "restore",
            Self::Delete => "delete",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "restore" => Self::Restore,
            "delete" => Self::Delete,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Restore,
            known => known.clone(),
        }
    }
}

impl Serialize for QuerySessionFileRewindPreviewResultSafeFilesItemAction {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QuerySessionFileRewindPreviewResultSafeFilesItemAction {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySessionFileRewindPreviewResultUnsafeFilesItem {
    #[serde(
        rename = "currentHash",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub current_hash: Option<String>,
    #[serde(
        rename = "expectedHash",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub expected_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(rename = "operationCount")]
    pub operation_count: u64,
    pub path: String,
    #[serde(
        default = "fallback_query_session_file_rewind_preview_result_unsafe_files_item_reason"
    )]
    pub reason: QuerySessionFileRewindPreviewResultUnsafeFilesItemReason,
    #[serde(
        rename = "toolNames",
        default = "default_query_session_file_rewind_preview_result_unsafe_files_item_tool_names"
    )]
    pub tool_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QuerySessionFileRewindPreviewResultUnsafeFilesItemReason {
    CheckpointMissing,
    CheckpointUnreadable,
    ExternalModified,
    FileReadFailed,
    UnsupportedCheckpoint,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QuerySessionFileRewindPreviewResultUnsafeFilesItemReason {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::UnsupportedCheckpoint;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::CheckpointMissing => "checkpoint_missing",
            Self::CheckpointUnreadable => "checkpoint_unreadable",
            Self::ExternalModified => "external_modified",
            Self::FileReadFailed => "file_read_failed",
            Self::UnsupportedCheckpoint => "unsupported_checkpoint",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "checkpoint_missing" => Self::CheckpointMissing,
            "checkpoint_unreadable" => Self::CheckpointUnreadable,
            "external_modified" => Self::ExternalModified,
            "file_read_failed" => Self::FileReadFailed,
            "unsupported_checkpoint" => Self::UnsupportedCheckpoint,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::UnsupportedCheckpoint,
            known => known.clone(),
        }
    }
}

impl Serialize for QuerySessionFileRewindPreviewResultUnsafeFilesItemReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QuerySessionFileRewindPreviewResultUnsafeFilesItemReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

pub type CommandTurnRetry = CommandFilesRewind;

fn fallback_command_result_turn_edit_disposition() -> CommandResultTurnEditDisposition {
    CommandResultTurnEditDisposition::Blocked
}

fn default_query_session_file_rewind_preview_result_ignored_files_item_tool_names() -> Vec<String> {
    Vec::new()
}

fn fallback_query_session_file_rewind_preview_result_safe_files_item_action()
-> QuerySessionFileRewindPreviewResultSafeFilesItemAction {
    QuerySessionFileRewindPreviewResultSafeFilesItemAction::Restore
}

fn default_query_session_file_rewind_preview_result_safe_files_item_tool_names() -> Vec<String> {
    Vec::new()
}

fn fallback_query_session_file_rewind_preview_result_unsafe_files_item_reason()
-> QuerySessionFileRewindPreviewResultUnsafeFilesItemReason {
    QuerySessionFileRewindPreviewResultUnsafeFilesItemReason::UnsupportedCheckpoint
}

fn default_query_session_file_rewind_preview_result_unsafe_files_item_tool_names() -> Vec<String> {
    Vec::new()
}
