//! 排版之前的摘要：992f0d48b280ea38
//! 生成物，不要手改：`agent@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults::{
    default_agent_cua_permission_request_missing,
    default_agent_cua_permission_request_permission_status_grant_owner,
    default_agent_cua_permission_request_permission_status_platform, default_agent_exit_code,
    default_agent_exit_signal, default_agent_state_build, default_agent_state_host_services,
    default_agent_state_migration_message, default_agent_state_phase,
    default_agent_state_providers, fallback_agent_cua_end_reason,
    fallback_agent_cua_permission_request_permission_status_accessibility,
    fallback_agent_cua_permission_request_permission_status_screen_recording,
    fallback_agent_exit_reason, fallback_agent_state_storage_migration_kind,
    fallback_agent_state_storage_phase,
};
use super::{AgentStateDiagnostics, AgentStateStorageMigrationKind};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCuaEnd {
    pub id: String,
    pub v: u64,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(default = "fallback_agent_cua_end_reason")]
    pub reason: AgentCuaEndReason,
    #[serde(rename = "endedAt")]
    pub ended_at: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentCuaEndReason {
    TurnCompleted,
    TurnFailed,
    SessionEnded,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl AgentCuaEndReason {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::TurnFailed;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::TurnCompleted => "turnCompleted",
            Self::TurnFailed => "turnFailed",
            Self::SessionEnded => "sessionEnded",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "turnCompleted" => Self::TurnCompleted,
            "turnFailed" => Self::TurnFailed,
            "sessionEnded" => Self::SessionEnded,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::TurnFailed,
            known => known.clone(),
        }
    }
}

impl Serialize for AgentCuaEndReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentCuaEndReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCuaOperation {
    pub id: String,
    pub v: u64,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    #[serde(rename = "startedAt")]
    pub started_at: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCuaPermissionRequest {
    pub id: String,
    pub v: u64,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "turnId", default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    #[serde(rename = "occurredAt")]
    pub occurred_at: f64,
    #[serde(default = "default_agent_cua_permission_request_missing")]
    pub missing: Vec<AgentCuaPermissionRequestMissingItem>,
    #[serde(rename = "permissionStatus")]
    pub permission_status: AgentCuaPermissionRequestPermissionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentCuaPermissionRequestMissingItem {
    Accessibility,
    ScreenRecording,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl AgentCuaPermissionRequestMissingItem {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Accessibility;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Accessibility => "accessibility",
            Self::ScreenRecording => "screen_recording",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "accessibility" => Self::Accessibility,
            "screen_recording" => Self::ScreenRecording,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Accessibility,
            known => known.clone(),
        }
    }
}

impl Serialize for AgentCuaPermissionRequestMissingItem {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentCuaPermissionRequestMissingItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCuaPermissionRequestPermissionStatus {
    #[serde(default = "default_agent_cua_permission_request_permission_status_platform")]
    pub platform: String,
    #[serde(
        rename = "grantOwner",
        default = "default_agent_cua_permission_request_permission_status_grant_owner"
    )]
    pub grant_owner: String,
    #[serde(default = "fallback_agent_cua_permission_request_permission_status_accessibility")]
    pub accessibility: AgentCuaPermissionRequestPermissionStatusAccessibility,
    #[serde(
        rename = "screenRecording",
        default = "fallback_agent_cua_permission_request_permission_status_screen_recording"
    )]
    pub screen_recording: AgentCuaPermissionRequestPermissionStatusScreenRecording,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentCuaPermissionRequestPermissionStatusAccessibility {
    Granted,
    Stale,
    Denied,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl AgentCuaPermissionRequestPermissionStatusAccessibility {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Denied;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Granted => "granted",
            Self::Stale => "stale",
            Self::Denied => "denied",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "granted" => Self::Granted,
            "stale" => Self::Stale,
            "denied" => Self::Denied,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Denied,
            known => known.clone(),
        }
    }
}

impl Serialize for AgentCuaPermissionRequestPermissionStatusAccessibility {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentCuaPermissionRequestPermissionStatusAccessibility {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentCuaPermissionRequestPermissionStatusScreenRecording {
    Granted,
    Denied,
    Unknown,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl AgentCuaPermissionRequestPermissionStatusScreenRecording {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unknown;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::Unknown => "unknown",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "granted" => Self::Granted,
            "denied" => Self::Denied,
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

impl Serialize for AgentCuaPermissionRequestPermissionStatusScreenRecording {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentCuaPermissionRequestPermissionStatusScreenRecording {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentExit {
    #[serde(default = "fallback_agent_exit_reason")]
    pub reason: AgentExitReason,
    #[serde(default = "default_agent_exit_code")]
    pub code: Option<i64>,
    #[serde(default = "default_agent_exit_signal")]
    pub signal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentExitReason {
    Restart,
    Release,
    Shutdown,
    Crashed,
    Exited,
    SpawnFailed,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl AgentExitReason {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Exited;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Restart => "restart",
            Self::Release => "release",
            Self::Shutdown => "shutdown",
            Self::Crashed => "crashed",
            Self::Exited => "exited",
            Self::SpawnFailed => "spawnFailed",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "restart" => Self::Restart,
            "release" => Self::Release,
            "shutdown" => Self::Shutdown,
            "crashed" => Self::Crashed,
            "exited" => Self::Exited,
            "spawnFailed" => Self::SpawnFailed,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Exited,
            known => known.clone(),
        }
    }
}

impl Serialize for AgentExitReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentExitReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentState {
    #[serde(default = "default_agent_state_phase")]
    pub phase: AgentStatePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<AgentStateMigration>,
    #[serde(default = "default_agent_state_providers")]
    pub providers: Vec<AgentStateProvidersItem>,
    #[serde(rename = "hostServices", default = "default_agent_state_host_services")]
    pub host_services: Vec<String>,
    #[serde(default = "default_agent_state_build")]
    pub build: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<AgentExit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<AgentStateStorage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<AgentStateDiagnostics>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentStatePhase {
    Starting,
    Migrating,
    Ready,
    Degraded,
    Stopped,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl AgentStatePhase {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Starting;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Starting => "starting",
            Self::Migrating => "migrating",
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::Stopped => "stopped",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "starting" => Self::Starting,
            "migrating" => Self::Migrating,
            "ready" => Self::Ready,
            "degraded" => Self::Degraded,
            "stopped" => Self::Stopped,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Starting,
            known => known.clone(),
        }
    }
}

impl Serialize for AgentStatePhase {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentStatePhase {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStateMigration {
    pub step: u64,
    pub total: u64,
    #[serde(default = "default_agent_state_migration_message")]
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStateProvidersItem {
    pub id: String,
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStateStorage {
    #[serde(rename = "databaseId")]
    pub database_id: String,
    #[serde(default = "fallback_agent_state_storage_phase")]
    pub phase: AgentStateStoragePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<AgentStateStorageMigration>,
    #[serde(rename = "errorCode", default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(
        rename = "sqliteCode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sqlite_code: Option<i64>,
    #[serde(
        rename = "systemCode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub system_code: Option<String>,
    #[serde(
        rename = "migrationId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub migration_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentStateStoragePhase {
    Checking,
    WaitingForLock,
    Migrating,
    Committing,
    Ready,
    Failed,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl AgentStateStoragePhase {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Checking;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Checking => "checking",
            Self::WaitingForLock => "waiting_for_lock",
            Self::Migrating => "migrating",
            Self::Committing => "committing",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "checking" => Self::Checking,
            "waiting_for_lock" => Self::WaitingForLock,
            "migrating" => Self::Migrating,
            "committing" => Self::Committing,
            "ready" => Self::Ready,
            "failed" => Self::Failed,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Checking,
            known => known.clone(),
        }
    }
}

impl Serialize for AgentStateStoragePhase {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentStateStoragePhase {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStateStorageMigration {
    #[serde(default = "fallback_agent_state_storage_migration_kind")]
    pub kind: AgentStateStorageMigrationKind,
    #[serde(rename = "executedCount")]
    pub executed_count: u64,
    #[serde(rename = "committedCount")]
    pub committed_count: u64,
    #[serde(
        rename = "lastAppliedMigrationId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_applied_migration_id: Option<String>,
}
