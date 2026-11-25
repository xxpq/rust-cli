//! 排版之前的摘要：e9f01a9432da6476
//! 生成物，不要手改：`session@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults_1::{
    default_interaction_answer_question_answers,
    default_interaction_hook_review_payload_bundle_digest,
    default_interaction_hook_review_payload_detail_created_at,
    default_interaction_hook_review_payload_detail_items,
    default_interaction_hook_review_payload_detail_source_files,
    default_interaction_hook_review_payload_detail_source_files_item_display_path,
    default_interaction_hook_review_payload_detail_source_files_item_editable,
    default_interaction_hook_review_payload_detail_summary_event_count,
    default_interaction_hook_review_payload_detail_summary_hook_count,
    default_interaction_hook_review_payload_detail_summary_pending_count,
    default_interaction_hook_review_payload_detail_workspace_label,
    default_interaction_hook_review_payload_items,
    default_interaction_hook_review_payload_items_item_configured_enabled,
    default_interaction_hook_review_payload_items_item_display_command,
    default_interaction_hook_review_payload_items_item_display_name,
    default_interaction_hook_review_payload_items_item_editable,
    default_interaction_hook_review_payload_items_item_event,
    default_interaction_hook_review_payload_items_item_source_path,
    default_interaction_mcp_elicitation_payload_fields,
    default_interaction_mcp_elicitation_payload_message, default_interaction_oauth_payload_state,
    default_interaction_permission_payload_full_access_option,
    default_interaction_permission_payload_options, default_interaction_permission_payload_summary,
    default_interaction_question_payload_questions,
    fallback_interaction_hook_review_payload_items_item_trust_state,
    fallback_interaction_mcp_elicitation_payload_mode,
};
use super::{
    AskedQuestion, ElicitationField, InteractionMcpElicitationPayloadMode, InteractionMcpSampling,
};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswerQuestion {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default = "default_interaction_answer_question_answers")]
    pub answers: BTreeMap<String, Opaque>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<BTreeMap<String, Opaque>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declined: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InteractionAnswerRequestVerification {
    Alt1(InteractionAnswerRequestVerificationAlt1),
    Alt2(InteractionAnswerRequestVerificationAlt2),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswerRequestVerificationAlt1 {
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswerRequestVerificationAlt2 {
    pub failure: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionOrigin {
    pub kind: InteractionOriginKind,
    #[serde(rename = "agentId")]
    pub agent_id: String,
    #[serde(rename = "agentType")]
    pub agent_type: String,
    #[serde(rename = "childSessionId")]
    pub child_session_id: String,
    pub description: String,
    #[serde(rename = "parentSessionId")]
    pub parent_session_id: String,
    #[serde(
        rename = "parentToolCallId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InteractionOriginKind {
    #[serde(rename = "subagent")]
    Subagent,
}

impl InteractionOriginKind {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Subagent => "subagent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Interaction {
    #[serde(rename = "permission")]
    Permission(InteractionPermission),
    #[serde(rename = "question")]
    Question(InteractionQuestion),
    #[serde(rename = "planApproval")]
    PlanApproval(InteractionPlanApproval),
    #[serde(rename = "hookReview")]
    HookReview(InteractionHookReview),
    #[serde(rename = "requestVerification")]
    RequestVerification(InteractionRequestVerification),
    #[serde(rename = "oauth")]
    Oauth(InteractionOauth),
    #[serde(rename = "mcpElicitation")]
    McpElicitation(InteractionMcpElicitation),
    #[serde(rename = "mcpSampling")]
    McpSampling(InteractionMcpSampling),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionPermission {
    pub id: String,
    pub v: u64,
    pub payload: InteractionPermissionPayload,
    #[serde(rename = "expiresAt", default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionPermissionPayload {
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    #[serde(default = "default_interaction_permission_payload_summary")]
    pub summary: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Opaque>,
    #[serde(default = "default_interaction_permission_payload_options")]
    pub options: Vec<InteractionPermissionPayloadOptionsItem>,
    #[serde(
        rename = "fullAccessOption",
        default = "default_interaction_permission_payload_full_access_option"
    )]
    pub full_access_option: bool,
    #[serde(
        rename = "modeSwitch",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub mode_switch: Option<InteractionPermissionPayloadModeSwitch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestions: Option<Vec<PermissionRule>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<InteractionOrigin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InteractionPermissionPayloadOptionsItem {
    AllowOnce,
    AllowAlways,
    Deny,
    Refine,
    FullAccess,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl InteractionPermissionPayloadOptionsItem {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Deny;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::AllowOnce => "allowOnce",
            Self::AllowAlways => "allowAlways",
            Self::Deny => "deny",
            Self::Refine => "refine",
            Self::FullAccess => "fullAccess",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "allowOnce" => Self::AllowOnce,
            "allowAlways" => Self::AllowAlways,
            "deny" => Self::Deny,
            "refine" => Self::Refine,
            "fullAccess" => Self::FullAccess,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Deny,
            known => known.clone(),
        }
    }
}

impl Serialize for InteractionPermissionPayloadOptionsItem {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InteractionPermissionPayloadOptionsItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InteractionPermissionPayloadModeSwitch {
    #[serde(rename = "choose")]
    Choose,
    #[serde(rename = "fullAccessOnly")]
    FullAccessOnly,
}

impl InteractionPermissionPayloadModeSwitch {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Choose => "choose",
            Self::FullAccessOnly => "fullAccessOnly",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionQuestion {
    pub id: String,
    pub v: u64,
    pub payload: InteractionQuestionPayload,
    #[serde(rename = "expiresAt", default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionQuestionPayload {
    #[serde(default = "default_interaction_question_payload_questions")]
    pub questions: Vec<AskedQuestion>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Opaque>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(
        rename = "toolCallId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_call_id: Option<String>,
    #[serde(rename = "toolName", default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<InteractionOrigin>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "autoResolution",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_resolution: Option<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionPlanApproval {
    pub id: String,
    pub v: u64,
    pub payload: InteractionPlanApprovalPayload,
    #[serde(rename = "expiresAt", default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionPlanApprovalPayload {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub plan: Opaque,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(
        rename = "toolCallId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_call_id: Option<String>,
    #[serde(rename = "toolName", default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<InteractionOrigin>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionHookReview {
    pub id: String,
    pub v: u64,
    pub payload: InteractionHookReviewPayload,
    #[serde(rename = "expiresAt", default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionHookReviewPayload {
    #[serde(rename = "flowId")]
    pub flow_id: String,
    #[serde(default = "default_interaction_hook_review_payload_items")]
    pub items: Vec<InteractionHookReviewPayloadItemsItem>,
    #[serde(
        rename = "bundleDigest",
        default = "default_interaction_hook_review_payload_bundle_digest"
    )]
    pub bundle_digest: String,
    #[serde(
        rename = "deadlineAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub deadline_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<InteractionHookReviewPayloadDetail>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionHookReviewPayloadItemsItem {
    #[serde(rename = "reviewItemId")]
    pub review_item_id: String,
    #[serde(default = "default_interaction_hook_review_payload_items_item_event")]
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<InteractionHookReviewPayloadItemsItemType>,
    #[serde(
        rename = "displayName",
        default = "default_interaction_hook_review_payload_items_item_display_name"
    )]
    pub display_name: String,
    #[serde(
        rename = "displayCommand",
        default = "default_interaction_hook_review_payload_items_item_display_command"
    )]
    pub display_command: String,
    #[serde(
        rename = "sourcePath",
        default = "default_interaction_hook_review_payload_items_item_source_path"
    )]
    pub source_path: String,
    #[serde(
        rename = "resolvedTimeoutMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub resolved_timeout_ms: Option<u64>,
    #[serde(
        rename = "resolvedMaxOutputBytes",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub resolved_max_output_bytes: Option<u64>,
    #[serde(
        rename = "executionMode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub execution_mode: Option<InteractionHookReviewPayloadItemsItemExecutionMode>,
    #[serde(
        rename = "configuredEnabled",
        default = "default_interaction_hook_review_payload_items_item_configured_enabled"
    )]
    pub configured_enabled: bool,
    #[serde(default = "default_interaction_hook_review_payload_items_item_editable")]
    pub editable: bool,
    #[serde(
        rename = "trustState",
        default = "fallback_interaction_hook_review_payload_items_item_trust_state"
    )]
    pub trust_state: InteractionHookReviewPayloadItemsItemTrustState,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InteractionHookReviewPayloadItemsItemType {
    Command,
    Process,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl InteractionHookReviewPayloadItemsItemType {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Command;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Command => "command",
            Self::Process => "process",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "command" => Self::Command,
            "process" => Self::Process,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Command,
            known => known.clone(),
        }
    }
}

impl Serialize for InteractionHookReviewPayloadItemsItemType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InteractionHookReviewPayloadItemsItemType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InteractionHookReviewPayloadItemsItemExecutionMode {
    Foreground,
    Background,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl InteractionHookReviewPayloadItemsItemExecutionMode {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Foreground;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "foreground" => Self::Foreground,
            "background" => Self::Background,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Foreground,
            known => known.clone(),
        }
    }
}

impl Serialize for InteractionHookReviewPayloadItemsItemExecutionMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InteractionHookReviewPayloadItemsItemExecutionMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InteractionHookReviewPayloadItemsItemTrustState {
    NotApplicable,
    PendingTrust,
    TrustedPersistent,
    BlockedUntrusted,
    BlockedPolicy,
    Revoked,
    StaleDigest,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl InteractionHookReviewPayloadItemsItemTrustState {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::BlockedUntrusted;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::NotApplicable => "not_applicable",
            Self::PendingTrust => "pending_trust",
            Self::TrustedPersistent => "trusted_persistent",
            Self::BlockedUntrusted => "blocked_untrusted",
            Self::BlockedPolicy => "blocked_policy",
            Self::Revoked => "revoked",
            Self::StaleDigest => "stale_digest",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "not_applicable" => Self::NotApplicable,
            "pending_trust" => Self::PendingTrust,
            "trusted_persistent" => Self::TrustedPersistent,
            "blocked_untrusted" => Self::BlockedUntrusted,
            "blocked_policy" => Self::BlockedPolicy,
            "revoked" => Self::Revoked,
            "stale_digest" => Self::StaleDigest,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::BlockedUntrusted,
            known => known.clone(),
        }
    }
}

impl Serialize for InteractionHookReviewPayloadItemsItemTrustState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InteractionHookReviewPayloadItemsItemTrustState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionHookReviewPayloadDetail {
    #[serde(rename = "reviewFlowId")]
    pub review_flow_id: String,
    pub generation: u64,
    #[serde(rename = "interactionId")]
    pub interaction_id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "taskId")]
    pub task_id: String,
    #[serde(rename = "runId")]
    pub run_id: String,
    #[serde(rename = "workspaceIdentity")]
    pub workspace_identity: String,
    #[serde(
        rename = "workspaceLabel",
        default = "default_interaction_hook_review_payload_detail_workspace_label"
    )]
    pub workspace_label: String,
    #[serde(
        rename = "remoteSessionId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub remote_session_id: Option<String>,
    #[serde(rename = "bundleDigest")]
    pub bundle_digest: String,
    #[serde(
        rename = "createdAt",
        default = "default_interaction_hook_review_payload_detail_created_at"
    )]
    pub created_at: f64,
    #[serde(
        rename = "deadlineAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub deadline_at: Option<f64>,
    #[serde(
        rename = "sourceFiles",
        default = "default_interaction_hook_review_payload_detail_source_files"
    )]
    pub source_files: Vec<InteractionHookReviewPayloadDetailSourceFilesItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<InteractionHookReviewPayloadDetailSummary>,
    #[serde(default = "default_interaction_hook_review_payload_detail_items")]
    pub items: Vec<InteractionHookReviewPayloadItemsItem>,
    #[serde(
        rename = "warningCode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub warning_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionHookReviewPayloadDetailSourceFilesItem {
    pub path: String,
    #[serde(
        rename = "displayPath",
        default = "default_interaction_hook_review_payload_detail_source_files_item_display_path"
    )]
    pub display_path: String,
    #[serde(default = "default_interaction_hook_review_payload_detail_source_files_item_editable")]
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionHookReviewPayloadDetailSummary {
    #[serde(
        rename = "eventCount",
        default = "default_interaction_hook_review_payload_detail_summary_event_count"
    )]
    pub event_count: u64,
    #[serde(
        rename = "hookCount",
        default = "default_interaction_hook_review_payload_detail_summary_hook_count"
    )]
    pub hook_count: u64,
    #[serde(
        rename = "pendingCount",
        default = "default_interaction_hook_review_payload_detail_summary_pending_count"
    )]
    pub pending_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionRequestVerification {
    pub id: String,
    pub v: u64,
    pub payload: InteractionRequestVerificationPayload,
    #[serde(rename = "expiresAt", default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionRequestVerificationPayload {
    pub provider: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub challenge: Opaque,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionOauth {
    pub id: String,
    pub v: u64,
    pub payload: InteractionOauthPayload,
    #[serde(rename = "expiresAt", default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionOauthPayload {
    #[serde(rename = "authorizeUrl")]
    pub authorize_url: String,
    #[serde(default = "default_interaction_oauth_payload_state")]
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionMcpElicitation {
    pub id: String,
    pub v: u64,
    pub payload: InteractionMcpElicitationPayload,
    #[serde(rename = "expiresAt", default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionMcpElicitationPayload {
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    #[serde(rename = "serverName")]
    pub server_name: String,
    #[serde(default = "default_interaction_mcp_elicitation_payload_message")]
    pub message: String,
    #[serde(default = "fallback_interaction_mcp_elicitation_payload_mode")]
    pub mode: InteractionMcpElicitationPayloadMode,
    #[serde(default = "default_interaction_mcp_elicitation_payload_fields")]
    pub fields: Vec<ElicitationField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<InteractionOrigin>,
}
