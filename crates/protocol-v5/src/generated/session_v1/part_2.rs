//! 排版之前的摘要：933a72bd1564c912
//! 生成物，不要手改：`session@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults_1::{
    default_elicitation_field_required, fallback_elicitation_field_type, fallback_input_fate_fate,
    fallback_interaction_answer_permission_option_id,
};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompactMarkerDetailOrigin {
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "auto")]
    Auto,
}

impl CompactMarkerDetailOrigin {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompactMarkerDetailStatus {
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "cancelled")]
    Cancelled,
}

impl CompactMarkerDetailStatus {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElicitationChoice {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElicitationField {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_elicitation_field_required")]
    pub required: bool,
    #[serde(default = "fallback_elicitation_field_type")]
    pub r#type: ElicitationFieldType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<ElicitationFieldFormat>,
    #[serde(rename = "minLength", default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    #[serde(rename = "maxLength", default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    #[serde(rename = "minItems", default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<u64>,
    #[serde(rename = "maxItems", default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ElicitationChoice>>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ElicitationFieldType {
    Text,
    Number,
    Integer,
    Boolean,
    Choice,
    Choices,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl ElicitationFieldType {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Text;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::Choice => "choice",
            Self::Choices => "choices",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "text" => Self::Text,
            "number" => Self::Number,
            "integer" => Self::Integer,
            "boolean" => Self::Boolean,
            "choice" => Self::Choice,
            "choices" => Self::Choices,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Text,
            known => known.clone(),
        }
    }
}

impl Serialize for ElicitationFieldType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ElicitationFieldType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ElicitationFieldFormat {
    #[serde(rename = "email")]
    Email,
    #[serde(rename = "uri")]
    Uri,
    #[serde(rename = "date")]
    Date,
    #[serde(rename = "dateTime")]
    DateTime,
}

impl ElicitationFieldFormat {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Uri => "uri",
            Self::Date => "date",
            Self::DateTime => "dateTime",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorAttribution {
    #[serde(
        rename = "highspeedCardId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub highspeed_card_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ExecutionCredential {
    #[serde(rename = "highspeedCard")]
    HighspeedCard(ExecutionCredentialHighspeedCard),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionCredentialHighspeedCard {
    #[serde(rename = "cardId")]
    pub card_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HighspeedRow {
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
    #[serde(
        rename = "outputTokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_tokens: Option<u64>,
    #[serde(
        rename = "durationMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub duration_ms: Option<f64>,
    #[serde(
        rename = "highspeedTps",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub highspeed_tps: Option<f64>,
    #[serde(
        rename = "savedDurationMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub saved_duration_ms: Option<f64>,
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
    #[serde(
        rename = "fallbackAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub fallback_at: Option<u64>,
    #[serde(
        rename = "fallbackReason",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub fallback_reason: Option<HighspeedRowFallbackReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HighspeedRowFallbackReason {
    HighspeedCardExpired,
    HighspeedRequestFailed,
    HighspeedSelectionUnavailable,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl HighspeedRowFallbackReason {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::HighspeedRequestFailed;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::HighspeedCardExpired => "highspeed_card_expired",
            Self::HighspeedRequestFailed => "highspeed_request_failed",
            Self::HighspeedSelectionUnavailable => "highspeed_selection_unavailable",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "highspeed_card_expired" => Self::HighspeedCardExpired,
            "highspeed_request_failed" => Self::HighspeedRequestFailed,
            "highspeed_selection_unavailable" => Self::HighspeedSelectionUnavailable,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::HighspeedRequestFailed,
            known => known.clone(),
        }
    }
}

impl Serialize for HighspeedRowFallbackReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HighspeedRowFallbackReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HighspeedTurnMeta {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HighspeedTurnMetrics {
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
pub struct InputFate {
    pub id: String,
    pub v: u64,
    #[serde(default = "fallback_input_fate_fate")]
    pub fate: InputFateFate,
    #[serde(rename = "turnId", default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InputFateFate {
    Queued,
    Started,
    Completed,
    Cancelled,
    Failed,
    Discarded,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl InputFateFate {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Failed;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Queued => "queued",
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Discarded => "discarded",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "queued" => Self::Queued,
            "started" => Self::Started,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            "failed" => Self::Failed,
            "discarded" => Self::Discarded,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Failed,
            known => known.clone(),
        }
    }
}

impl Serialize for InputFateFate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InputFateFate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputQuote {
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
pub enum InputSource {
    #[serde(rename = "pluginUi")]
    PluginUi(InputSourcePluginUi),
    #[serde(rename = "genUi")]
    GenUi(InputSourceGenUi),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputSourcePluginUi {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputSourceGenUi {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswerHookReview {
    pub decisions: Vec<InteractionAnswerHookReviewDecisionsItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswerHookReviewDecisionsItem {
    pub action: InteractionAnswerHookReviewDecisionsItemAction,
    #[serde(rename = "reviewItemIds")]
    pub review_item_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InteractionAnswerHookReviewDecisionsItemAction {
    #[serde(rename = "trust_selected")]
    TrustSelected,
}

impl InteractionAnswerHookReviewDecisionsItemAction {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TrustSelected => "trust_selected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswerMcpElicitation {
    pub action: InteractionAnswerMcpElicitationAction,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<BTreeMap<String, Opaque>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InteractionAnswerMcpElicitationAction {
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "decline")]
    Decline,
    #[serde(rename = "cancel")]
    Cancel,
}

impl InteractionAnswerMcpElicitationAction {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Decline => "decline",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswerMcpSampling {
    pub allow: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswerOauth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswerPermission {
    #[serde(
        rename = "optionId",
        default = "fallback_interaction_answer_permission_option_id"
    )]
    pub option_id: InteractionAnswerPermissionOptionId,
    #[serde(
        rename = "refineText",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub refine_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "inputAdjustments",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_adjustments: Option<BTreeMap<String, Opaque>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InteractionAnswerPermissionOptionId {
    AllowOnce,
    AllowAlways,
    Deny,
    Refine,
    FullAccess,
    AllowAndSwitchGuarded,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl InteractionAnswerPermissionOptionId {
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
            Self::AllowAndSwitchGuarded => "allowAndSwitchGuarded",
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
            "allowAndSwitchGuarded" => Self::AllowAndSwitchGuarded,
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

impl Serialize for InteractionAnswerPermissionOptionId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InteractionAnswerPermissionOptionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswerPlanApproval {
    pub approved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
}
