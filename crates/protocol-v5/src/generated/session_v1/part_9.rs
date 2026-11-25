//! 排版之前的摘要：5b6c12a31db219a8
//! 生成物，不要手改：`session@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults_2::{
    default_session_state_config_followup_mode, default_session_state_config_mode,
    default_session_state_config_model, default_session_state_config_plan_enabled,
    default_session_state_config_provider, default_session_state_config_thought,
    default_session_state_control_api_retry_reason, default_session_state_goal_iteration,
    default_session_state_goal_iterations, default_session_state_goal_status,
    default_session_state_goal_target_id, default_session_state_goal_verifications,
    default_session_state_meta_created_at, default_session_state_meta_kind,
    default_session_state_meta_title, default_session_state_meta_title_source,
    default_session_state_plan_items, default_session_state_plan_items_item_status,
    default_session_state_plan_items_item_text, default_session_state_plan_updated_at,
    default_session_state_queue_policy_auto_drain, default_session_state_shared_context_status,
    default_session_state_shared_context_title, default_session_state_usage_context_window,
    default_session_state_usage_cumulative, default_session_state_usage_cumulative_cache_read,
    default_session_state_usage_cumulative_cache_write,
    default_session_state_usage_cumulative_input_tokens,
    default_session_state_usage_cumulative_output_tokens,
};
use super::{
    RowUserInputModelSelection, SessionFactFieldsTitleSource, SessionStateQueuePolicyPauseReason,
};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateControlLastErrorSource {
    Provider,
    Runtime,
    Tool,
    Network,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateControlLastErrorSource {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Runtime;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Provider => "provider",
            Self::Runtime => "runtime",
            Self::Tool => "tool",
            Self::Network => "network",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "provider" => Self::Provider,
            "runtime" => Self::Runtime,
            "tool" => Self::Tool,
            "network" => Self::Network,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Runtime,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionStateControlLastErrorSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateControlLastErrorSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateControlApiRetry {
    pub attempt: u64,
    pub max: u64,
    #[serde(default = "default_session_state_control_api_retry_reason")]
    pub reason: String,
    #[serde(
        rename = "nextRetryAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub next_retry_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateControlLastOutcome {
    Success,
    Interrupted,
    Error,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateControlLastOutcome {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Success;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Success => "success",
            Self::Interrupted => "interrupted",
            Self::Error => "error",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "success" => Self::Success,
            "interrupted" => Self::Interrupted,
            "error" => Self::Error,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Success,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionStateControlLastOutcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateControlLastOutcome {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateMeta {
    #[serde(default = "default_session_state_meta_title")]
    pub title: String,
    #[serde(
        rename = "titleSource",
        default = "default_session_state_meta_title_source"
    )]
    pub title_source: SessionFactFieldsTitleSource,
    #[serde(
        rename = "createdAt",
        default = "default_session_state_meta_created_at"
    )]
    pub created_at: f64,
    #[serde(default = "default_session_state_meta_kind")]
    pub kind: SessionStateMetaKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateMetaKind {
    Main,
    SideChat,
    Child,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateMetaKind {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Main;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Main => "main",
            Self::SideChat => "sideChat",
            Self::Child => "child",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "main" => Self::Main,
            "sideChat" => Self::SideChat,
            "child" => Self::Child,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Main,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionStateMetaKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateMetaKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateConfig {
    #[serde(default = "default_session_state_config_provider")]
    pub provider: String,
    #[serde(default = "default_session_state_config_model")]
    pub model: String,
    #[serde(default = "default_session_state_config_thought")]
    pub thought: String,
    #[serde(default = "default_session_state_config_mode")]
    pub mode: String,
    #[serde(
        rename = "followupMode",
        default = "default_session_state_config_followup_mode"
    )]
    pub followup_mode: SessionStateConfigFollowupMode,
    #[serde(
        rename = "planEnabled",
        default = "default_session_state_config_plan_enabled"
    )]
    pub plan_enabled: bool,
    #[serde(
        rename = "modelSelection",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_selection: Option<RowUserInputModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<SessionStateConfigFallback>,
    #[serde(
        rename = "permissionGrant",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_grant: Option<SessionStateConfigPermissionGrant>,
    #[serde(
        rename = "planTransition",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_transition: Option<SessionStateConfigPlanTransition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateConfigFollowupMode {
    Queue,
    Guide,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateConfigFollowupMode {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Queue;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Queue => "queue",
            Self::Guide => "guide",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
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

impl Serialize for SessionStateConfigFollowupMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateConfigFollowupMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateConfigFallback {
    pub from: String,
    pub to: String,
    #[serde(rename = "eventId", default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateConfigPermissionGrant {
    #[serde(rename = "interactionId")]
    pub interaction_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateConfigPlanTransition {
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    #[serde(rename = "planEnabled")]
    pub plan_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateUsage {
    #[serde(
        rename = "contextWindow",
        default = "default_session_state_usage_context_window"
    )]
    pub context_window: Option<SessionStateUsageContextWindow>,
    #[serde(default = "default_session_state_usage_cumulative")]
    pub cumulative: SessionStateUsageCumulative,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateUsageContextWindow {
    pub used: f64,
    pub max: f64,
    #[serde(
        rename = "autoCompactThreshold",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_compact_threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<f64>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(rename = "cacheHit", default, skip_serializing_if = "Option::is_none")]
    pub cache_hit: Option<Opaque>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateUsageCumulative {
    #[serde(
        rename = "inputTokens",
        default = "default_session_state_usage_cumulative_input_tokens"
    )]
    pub input_tokens: f64,
    #[serde(
        rename = "outputTokens",
        default = "default_session_state_usage_cumulative_output_tokens"
    )]
    pub output_tokens: f64,
    #[serde(
        rename = "cacheRead",
        default = "default_session_state_usage_cumulative_cache_read"
    )]
    pub cache_read: f64,
    #[serde(
        rename = "cacheWrite",
        default = "default_session_state_usage_cumulative_cache_write"
    )]
    pub cache_write: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateGoal {
    #[serde(rename = "targetId", default = "default_session_state_goal_target_id")]
    pub target_id: Option<String>,
    #[serde(default = "default_session_state_goal_status")]
    pub status: SessionStateGoalStatus,
    #[serde(default = "default_session_state_goal_iteration")]
    pub iteration: u64,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default = "default_session_state_goal_verifications")]
    pub verifications: Vec<Opaque>,
    #[serde(default = "default_session_state_goal_iterations")]
    pub iterations: Vec<SessionStateGoalIterationsItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(
        rename = "summaryTitle",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub summary_title: Option<String>,
    #[serde(
        rename = "timeUsedSeconds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub time_used_seconds: Option<f64>,
    #[serde(
        rename = "activeRunStartedAtMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub active_run_started_at_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateGoalStatus {
    None,
    Active,
    Verifying,
    Verified,
    NotSatisfied,
    Paused,
    Failed,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateGoalStatus {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::None;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Active => "active",
            Self::Verifying => "verifying",
            Self::Verified => "verified",
            Self::NotSatisfied => "notSatisfied",
            Self::Paused => "paused",
            Self::Failed => "failed",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "none" => Self::None,
            "active" => Self::Active,
            "verifying" => Self::Verifying,
            "verified" => Self::Verified,
            "notSatisfied" => Self::NotSatisfied,
            "paused" => Self::Paused,
            "failed" => Self::Failed,
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

impl Serialize for SessionStateGoalStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateGoalStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateGoalIterationsItem {
    pub index: i64,
    #[serde(rename = "planSnapshotId")]
    pub plan_snapshot_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStatePlan {
    #[serde(default = "default_session_state_plan_items")]
    pub items: Vec<SessionStatePlanItemsItem>,
    #[serde(
        rename = "updatedAt",
        default = "default_session_state_plan_updated_at"
    )]
    pub updated_at: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStatePlanItemsItem {
    pub id: String,
    #[serde(default = "default_session_state_plan_items_item_text")]
    pub text: String,
    #[serde(default = "default_session_state_plan_items_item_status")]
    pub status: SessionStatePlanItemsItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStatePlanItemsItemStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStatePlanItemsItemStatus {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Pending;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Pending,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionStatePlanItemsItemStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStatePlanItemsItemStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateSharedContext {
    #[serde(rename = "contextId")]
    pub context_id: String,
    #[serde(default = "default_session_state_shared_context_title")]
    pub title: String,
    #[serde(rename = "shareUrl", default, skip_serializing_if = "Option::is_none")]
    pub share_url: Option<String>,
    #[serde(default = "default_session_state_shared_context_status")]
    pub status: SessionStateSharedContextStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateSharedContextStatus {
    Pending,
    Reserved,
    Attached,
    Discarded,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateSharedContextStatus {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Pending;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::Reserved => "reserved",
            Self::Attached => "attached",
            Self::Discarded => "discarded",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "pending" => Self::Pending,
            "reserved" => Self::Reserved,
            "attached" => Self::Attached,
            "discarded" => Self::Discarded,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Pending,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionStateSharedContextStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateSharedContextStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateQueuePolicy {
    #[serde(
        rename = "autoDrain",
        default = "default_session_state_queue_policy_auto_drain"
    )]
    pub auto_drain: bool,
    #[serde(
        rename = "pauseReason",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub pause_reason: Option<SessionStateQueuePolicyPauseReason>,
}
