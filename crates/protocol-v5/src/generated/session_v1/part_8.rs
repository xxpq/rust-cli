//! 排版之前的摘要：f004f166422a6e7e
//! 生成物，不要手改：`session@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults_1::{
    default_service_host_preferences_get_result_ask_user_question_auto_resolution_enabled,
    default_service_host_preferences_get_result_memory_enabled,
    default_service_host_preferences_get_result_model_context_budget_strategy,
    default_service_host_preferences_get_result_native_search_enhancements_enabled,
    default_service_host_preferences_get_result_tool_search_mode, default_session_fact_config,
    default_session_fact_created_at, default_session_fact_deleted_at,
    default_session_fact_last_outcome, default_session_fact_title,
    default_session_fact_title_source, default_session_fact_updated_at,
    default_session_state_control_phase, default_session_state_control_runtime,
    default_session_state_control_turn_id,
};
use super::defaults_2::{
    default_session_state_admission, default_session_state_config, default_session_state_control,
    default_session_state_control_active_works,
    default_session_state_control_active_works_item_started_at_seq,
    default_session_state_control_api_retry, default_session_state_control_last_error,
    default_session_state_control_last_error_message, default_session_state_control_last_outcome,
    default_session_state_goal, default_session_state_hook_admission,
    default_session_state_mcp_admission, default_session_state_meta, default_session_state_notices,
    default_session_state_plan, default_session_state_queue_order,
    default_session_state_queue_policy, default_session_state_rows_window,
    default_session_state_shared_context, default_session_state_usage,
    fallback_session_state_control_active_works_item_kind,
};
use super::{
    ErrorAttribution, ExecutionCredential, SessionFactFieldsConfig, SessionFactFieldsLastOutcome,
    SessionFactFieldsMigrationSource, SessionFactFieldsSummary, SessionFactFieldsTitleSource,
    SessionStateAdmission, SessionStateConfig, SessionStateControlApiRetry,
    SessionStateControlLastErrorSource, SessionStateControlLastOutcome, SessionStateGoal,
    SessionStateHookAdmission, SessionStateMeta, SessionStateNoticesItem, SessionStatePlan,
    SessionStateQueuePolicy, SessionStateRowsWindow, SessionStateSharedContext, SessionStateUsage,
};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostCredentialsMcpAuthHeadersParams {
    #[serde(rename = "mcpServer")]
    pub mcp_server: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(rename = "pluginId", default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(
        rename = "targetOrigin",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub target_origin: Option<String>,
    #[serde(
        rename = "workspaceKey",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_key: Option<String>,
    #[serde(
        rename = "workspacePath",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_path: Option<String>,
    #[serde(
        rename = "workspaceIdentity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_identity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostCredentialsMcpAuthHeadersResult {
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostOffPeakCreateResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(rename = "taskId", default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Opaque>,
    #[serde(
        rename = "failureStage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub failure_stage: Option<String>,
    #[serde(
        rename = "errorCategory",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub error_category: Option<String>,
    #[serde(rename = "errorCode", default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostOffPeakListResult {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub tasks: Vec<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostPreferencesGetResult {
    #[serde(
        rename = "nativeSearchEnhancementsEnabled",
        default = "default_service_host_preferences_get_result_native_search_enhancements_enabled"
    )]
    pub native_search_enhancements_enabled: bool,
    #[serde(
        rename = "memoryEnabled",
        default = "default_service_host_preferences_get_result_memory_enabled"
    )]
    pub memory_enabled: bool,
    #[serde(
        rename = "toolSearchMode",
        default = "default_service_host_preferences_get_result_tool_search_mode"
    )]
    pub tool_search_mode: ServiceHostPreferencesGetResultToolSearchMode,
    #[serde(
        rename = "askUserQuestionAutoResolutionEnabled",
        default = "default_service_host_preferences_get_result_ask_user_question_auto_resolution_enabled"
    )]
    pub ask_user_question_auto_resolution_enabled: bool,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "integratedTerminalShell",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub integrated_terminal_shell: Option<Opaque>,
    #[serde(
        rename = "modelContextBudgetStrategy",
        default = "default_service_host_preferences_get_result_model_context_budget_strategy"
    )]
    pub model_context_budget_strategy: ServiceHostPreferencesGetResultModelContextBudgetStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ServiceHostPreferencesGetResultToolSearchMode {
    Auto,
    Local,
    Off,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl ServiceHostPreferencesGetResultToolSearchMode {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Off;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Auto => "auto",
            Self::Local => "local",
            Self::Off => "off",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "auto" => Self::Auto,
            "local" => Self::Local,
            "off" => Self::Off,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Off,
            known => known.clone(),
        }
    }
}

impl Serialize for ServiceHostPreferencesGetResultToolSearchMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ServiceHostPreferencesGetResultToolSearchMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ServiceHostPreferencesGetResultModelContextBudgetStrategy {
    Legacy,
    PreflightV1,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl ServiceHostPreferencesGetResultModelContextBudgetStrategy {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::PreflightV1;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Legacy => "legacy",
            Self::PreflightV1 => "preflight-v1",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "legacy" => Self::Legacy,
            "preflight-v1" => Self::PreflightV1,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::PreflightV1,
            known => known.clone(),
        }
    }
}

impl Serialize for ServiceHostPreferencesGetResultModelContextBudgetStrategy {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ServiceHostPreferencesGetResultModelContextBudgetStrategy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostProvidersRequestAuthParams {
    pub provider: String,
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(rename = "turnId", default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(rename = "modelId", default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(
        rename = "accountAccess",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub account_access: Option<Opaque>,
    #[serde(
        rename = "verificationToken",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub verification_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected: Option<ServiceHostProvidersRequestAuthParamsRejected>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied: Option<ServiceHostProvidersRequestAuthParamsDenied>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionCredential>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostProvidersRequestAuthParamsRejected {
    pub fingerprint: String,
    #[serde(
        rename = "accountScope",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub account_scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostProvidersRequestAuthParamsDenied {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ServiceHostProvidersRequestAuthResult {
    #[serde(rename = "headers")]
    Headers(ServiceHostProvidersRequestAuthResultHeaders),
    #[serde(rename = "needsUser")]
    NeedsUser(ServiceHostProvidersRequestAuthResultNeedsUser),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostProvidersRequestAuthResultHeaders {
    pub headers: BTreeMap<String, String>,
    #[serde(rename = "apiKey", default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(
        rename = "accountScope",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub account_scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostProvidersRequestAuthResultNeedsUser {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge: Option<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostSessionExecutionDirectoriesResult {
    #[serde(rename = "executionDirectories")]
    pub execution_directories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFact {
    pub id: String,
    pub v: u64,
    #[serde(default = "default_session_fact_title")]
    pub title: String,
    #[serde(rename = "titleSource", default = "default_session_fact_title_source")]
    pub title_source: SessionFactFieldsTitleSource,
    #[serde(rename = "createdAt", default = "default_session_fact_created_at")]
    pub created_at: f64,
    #[serde(rename = "updatedAt", default = "default_session_fact_updated_at")]
    pub updated_at: f64,
    #[serde(rename = "lastOutcome", default = "default_session_fact_last_outcome")]
    pub last_outcome: Option<SessionFactFieldsLastOutcome>,
    #[serde(rename = "deletedAt", default = "default_session_fact_deleted_at")]
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
    #[serde(default = "default_session_fact_config")]
    pub config: SessionFactFieldsConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SessionFactFieldsSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    #[serde(default = "default_session_state_control")]
    pub control: SessionStateControl,
    #[serde(default = "default_session_state_meta")]
    pub meta: SessionStateMeta,
    #[serde(default = "default_session_state_config")]
    pub config: SessionStateConfig,
    #[serde(default = "default_session_state_usage")]
    pub usage: SessionStateUsage,
    #[serde(default = "default_session_state_goal")]
    pub goal: SessionStateGoal,
    #[serde(default = "default_session_state_plan")]
    pub plan: SessionStatePlan,
    #[serde(
        rename = "sharedContext",
        default = "default_session_state_shared_context"
    )]
    pub shared_context: Option<SessionStateSharedContext>,
    #[serde(rename = "queuePolicy", default = "default_session_state_queue_policy")]
    pub queue_policy: SessionStateQueuePolicy,
    #[serde(default = "default_session_state_admission")]
    pub admission: SessionStateAdmission,
    #[serde(
        rename = "hookAdmission",
        default = "default_session_state_hook_admission"
    )]
    pub hook_admission: Option<SessionStateHookAdmission>,
    #[serde(
        rename = "mcpAdmission",
        default = "default_session_state_mcp_admission"
    )]
    pub mcp_admission: Option<SessionStateHookAdmission>,
    #[serde(rename = "rowsWindow", default = "default_session_state_rows_window")]
    pub rows_window: SessionStateRowsWindow,
    #[serde(rename = "queueOrder", default = "default_session_state_queue_order")]
    pub queue_order: Vec<String>,
    #[serde(default = "default_session_state_notices")]
    pub notices: Vec<SessionStateNoticesItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateControl {
    #[serde(default = "default_session_state_control_phase")]
    pub phase: SessionStateControlPhase,
    #[serde(default = "default_session_state_control_runtime")]
    pub runtime: SessionStateControlRuntime,
    #[serde(rename = "turnId", default = "default_session_state_control_turn_id")]
    pub turn_id: Option<String>,
    #[serde(
        rename = "activeWorks",
        default = "default_session_state_control_active_works"
    )]
    pub active_works: Vec<SessionStateControlActiveWorksItem>,
    #[serde(
        rename = "lastError",
        default = "default_session_state_control_last_error"
    )]
    pub last_error: Option<SessionStateControlLastError>,
    #[serde(
        rename = "apiRetry",
        default = "default_session_state_control_api_retry"
    )]
    pub api_retry: Option<SessionStateControlApiRetry>,
    #[serde(
        rename = "lastOutcome",
        default = "default_session_state_control_last_outcome"
    )]
    pub last_outcome: Option<SessionStateControlLastOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateControlPhase {
    Draft,
    Idle,
    Running,
    Completed,
    Error,
    Deleted,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateControlPhase {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Idle;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Draft => "draft",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Deleted => "deleted",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "draft" => Self::Draft,
            "idle" => Self::Idle,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "error" => Self::Error,
            "deleted" => Self::Deleted,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Idle,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionStateControlPhase {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateControlPhase {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateControlRuntime {
    Cold,
    Starting,
    Ready,
    Busy,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateControlRuntime {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Cold;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Cold => "cold",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Busy => "busy",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "cold" => Self::Cold,
            "starting" => Self::Starting,
            "ready" => Self::Ready,
            "busy" => Self::Busy,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Cold,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionStateControlRuntime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateControlRuntime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateControlActiveWorksItem {
    pub id: String,
    #[serde(default = "fallback_session_state_control_active_works_item_kind")]
    pub kind: SessionStateControlActiveWorksItemKind,
    #[serde(
        rename = "startedAtSeq",
        default = "default_session_state_control_active_works_item_started_at_seq"
    )]
    pub started_at_seq: u64,
    #[serde(rename = "startedAt", default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateControlActiveWorksItemKind {
    Turn,
    Compact,
    GoalVerify,
    Tool,
    Subagent,
    GoalContinuation,
    Unknown,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateControlActiveWorksItemKind {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unknown;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Turn => "turn",
            Self::Compact => "compact",
            Self::GoalVerify => "goalVerify",
            Self::Tool => "tool",
            Self::Subagent => "subagent",
            Self::GoalContinuation => "goalContinuation",
            Self::Unknown => "unknown",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "turn" => Self::Turn,
            "compact" => Self::Compact,
            "goalVerify" => Self::GoalVerify,
            "tool" => Self::Tool,
            "subagent" => Self::Subagent,
            "goalContinuation" => Self::GoalContinuation,
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

impl Serialize for SessionStateControlActiveWorksItemKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateControlActiveWorksItemKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateControlLastError {
    pub code: String,
    #[serde(default = "default_session_state_control_last_error_message")]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recoverable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SessionStateControlLastErrorSource>,
    #[serde(rename = "traceId", default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(
        rename = "underlyingErrorMessage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub underlying_error_message: Option<String>,
    #[serde(
        rename = "underlyingErrorDetail",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub underlying_error_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<ErrorAttribution>,
}
