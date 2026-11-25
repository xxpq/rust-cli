//! 排版之前的摘要：d0dab7241cad7227
//! 生成物，不要手改：`session@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults_2::{
    default_session_state_admission_compact, default_session_state_admission_edit,
    default_session_state_admission_fork, default_session_state_admission_goal_pause,
    default_session_state_admission_goal_resume, default_session_state_admission_input,
    default_session_state_admission_input_allowed, default_session_state_admission_input_mode,
    default_session_state_admission_queue_send_now, default_session_state_admission_retry,
    default_session_state_admission_rewind_files, default_session_state_admission_stop,
    default_session_state_hook_admission_bundle_digest, default_session_state_rows_window_earlier,
    default_session_state_rows_window_first_id, default_session_state_rows_window_total_count,
    default_work_started_at_seq, default_work_status, default_work_title,
    fallback_session_state_notices_item_level, fallback_work_kind,
};
use super::{
    CommandInteractionSnooze, CommandResultSessionCreate, QuerySessionDebugParams,
    QuerySessionFileChangesParams,
};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateQueuePolicyPauseReason {
    Stopped,
    TurnFailed,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateQueuePolicyPauseReason {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Stopped;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Stopped => "stopped",
            Self::TurnFailed => "turnFailed",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "stopped" => Self::Stopped,
            "turnFailed" => Self::TurnFailed,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Stopped,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionStateQueuePolicyPauseReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateQueuePolicyPauseReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateAdmission {
    #[serde(default = "default_session_state_admission_input")]
    pub input: SessionStateAdmissionInput,
    #[serde(default = "default_session_state_admission_stop")]
    pub stop: SessionStateAdmissionStop,
    #[serde(default = "default_session_state_admission_compact")]
    pub compact: SessionStateAdmissionStop,
    #[serde(default = "default_session_state_admission_fork")]
    pub fork: SessionStateAdmissionStop,
    #[serde(default = "default_session_state_admission_edit")]
    pub edit: SessionStateAdmissionStop,
    #[serde(default = "default_session_state_admission_retry")]
    pub retry: SessionStateAdmissionStop,
    #[serde(
        rename = "rewindFiles",
        default = "default_session_state_admission_rewind_files"
    )]
    pub rewind_files: SessionStateAdmissionStop,
    #[serde(
        rename = "queueSendNow",
        default = "default_session_state_admission_queue_send_now"
    )]
    pub queue_send_now: SessionStateAdmissionStop,
    #[serde(
        rename = "goalPause",
        default = "default_session_state_admission_goal_pause"
    )]
    pub goal_pause: SessionStateAdmissionStop,
    #[serde(
        rename = "goalResume",
        default = "default_session_state_admission_goal_resume"
    )]
    pub goal_resume: SessionStateAdmissionStop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateAdmissionInput {
    #[serde(default = "default_session_state_admission_input_mode")]
    pub mode: SessionStateAdmissionInputMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default = "default_session_state_admission_input_allowed")]
    pub allowed: Vec<SessionStateAdmissionInputAllowedItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateAdmissionInputMode {
    StartNow,
    Enqueue,
    Guide,
    Choice,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateAdmissionInputMode {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::StartNow;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::StartNow => "startNow",
            Self::Enqueue => "enqueue",
            Self::Guide => "guide",
            Self::Choice => "choice",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "startNow" => Self::StartNow,
            "enqueue" => Self::Enqueue,
            "guide" => Self::Guide,
            "choice" => Self::Choice,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::StartNow,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionStateAdmissionInputMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateAdmissionInputMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateAdmissionInputAllowedItem {
    StartNow,
    Enqueue,
    Guide,
    KeepQueue,
    DiscardQueue,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateAdmissionInputAllowedItem {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Enqueue;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::StartNow => "startNow",
            Self::Enqueue => "enqueue",
            Self::Guide => "guide",
            Self::KeepQueue => "keepQueue",
            Self::DiscardQueue => "discardQueue",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "startNow" => Self::StartNow,
            "enqueue" => Self::Enqueue,
            "guide" => Self::Guide,
            "keepQueue" => Self::KeepQueue,
            "discardQueue" => Self::DiscardQueue,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Enqueue,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionStateAdmissionInputAllowedItem {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateAdmissionInputAllowedItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateAdmissionStop {
    pub allow: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateHookAdmission {
    #[serde(rename = "pendingCount")]
    pub pending_count: u64,
    #[serde(
        rename = "bundleDigest",
        default = "default_session_state_hook_admission_bundle_digest"
    )]
    pub bundle_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateRowsWindow {
    #[serde(
        rename = "totalCount",
        default = "default_session_state_rows_window_total_count"
    )]
    pub total_count: u64,
    #[serde(
        rename = "firstId",
        default = "default_session_state_rows_window_first_id"
    )]
    pub first_id: Option<String>,
    #[serde(default = "default_session_state_rows_window_earlier")]
    pub earlier: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStateNoticesItem {
    pub id: String,
    #[serde(default = "fallback_session_state_notices_item_level")]
    pub level: SessionStateNoticesItemLevel,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<SessionStateNoticesItemKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStateNoticesItemLevel {
    Info,
    Warning,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl SessionStateNoticesItemLevel {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Info;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "info" => Self::Info,
            "warning" => Self::Warning,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Info,
            known => known.clone(),
        }
    }
}

impl Serialize for SessionStateNoticesItemLevel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStateNoticesItemLevel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionStateNoticesItemKind {
    #[serde(rename = "stopHookLimit")]
    StopHookLimit,
    #[serde(rename = "steerBlocked")]
    SteerBlocked,
}

impl SessionStateNoticesItemKind {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StopHookLimit => "stopHookLimit",
            Self::SteerBlocked => "steerBlocked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Work {
    pub id: String,
    pub v: u64,
    #[serde(default = "fallback_work_kind")]
    pub kind: WorkKind,
    #[serde(default = "default_work_status")]
    pub status: WorkStatus,
    #[serde(default = "default_work_title")]
    pub title: String,
    #[serde(
        rename = "anchorRowId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub anchor_row_id: Option<String>,
    #[serde(rename = "startedAtSeq", default = "default_work_started_at_seq")]
    pub started_at_seq: u64,
    #[serde(rename = "startedAt", default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WorkKind {
    Bash,
    Subagent,
    WorkflowRun,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl WorkKind {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Bash;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Bash => "bash",
            Self::Subagent => "subagent",
            Self::WorkflowRun => "workflowRun",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "bash" => Self::Bash,
            "subagent" => Self::Subagent,
            "workflowRun" => Self::WorkflowRun,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Bash,
            known => known.clone(),
        }
    }
}

impl Serialize for WorkKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WorkKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WorkStatus {
    Running,
    Cancelling,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl WorkStatus {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Running;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "running" => Self::Running,
            "cancelling" => Self::Cancelling,
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

impl Serialize for WorkStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WorkStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

pub type CommandSessionDelete = CommandInteractionSnooze;

pub type CommandWorkCancel = CommandInteractionSnooze;

pub type QuerySessionPlansParams = QuerySessionDebugParams;

pub type QueryToolOutputParams = QuerySessionFileChangesParams;

pub type ServiceHostAutomationCreateParams = BTreeMap<String, Opaque>;

pub type ServiceHostAutomationListParams = QuerySessionDebugParams;

pub type ServiceHostBrowserExecuteResult = Opaque;

pub type ServiceHostOffPeakCreateParams = BTreeMap<String, Opaque>;

pub type ServiceHostOffPeakListParams = QuerySessionDebugParams;

pub type ServiceHostPreferencesGetParams = QuerySessionDebugParams;

pub type ServiceHostSessionExecutionDirectoriesParams = CommandResultSessionCreate;

pub type SessionCommandId = String;
