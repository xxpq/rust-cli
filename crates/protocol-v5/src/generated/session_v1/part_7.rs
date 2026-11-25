//! 排版之前的摘要：5663091cc16ae8ed
//! 生成物，不要手改：`session@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults_1::{
    default_queue_item_attachments, default_queue_item_dispatch, default_queue_item_dispatch_state,
    default_queue_item_kind, default_queue_item_order_admission_seq, default_queue_item_text,
    fallback_query_work_bash_output_result_alt1_status,
    fallback_query_work_bash_output_result_alt2_kind,
    fallback_service_host_credentials_cipher_result_failed_reason,
};
use super::{InputQuote, RowUserInputModelSelection};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum QueryWorkBashOutputResult {
    Alt1(QueryWorkBashOutputResultAlt1),
    Alt2(QueryWorkBashOutputResultAlt2),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryWorkBashOutputResultAlt1 {
    pub kind: QueryWorkBashOutputResultAlt1Kind,
    #[serde(rename = "workId")]
    pub work_id: String,
    #[serde(default = "fallback_query_work_bash_output_result_alt1_status")]
    pub status: QueryWorkBashOutputResultAlt1Status,
    pub output: String,
    pub truncated: bool,
    #[serde(rename = "outputPath")]
    pub output_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QueryWorkBashOutputResultAlt1Kind {
    #[serde(rename = "output")]
    Output,
}

impl QueryWorkBashOutputResultAlt1Kind {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Output => "output",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QueryWorkBashOutputResultAlt1Status {
    Running,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
    SpawnError,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QueryWorkBashOutputResultAlt1Status {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Running;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::SpawnError => "spawn_error",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "timed_out" => Self::TimedOut,
            "cancelled" => Self::Cancelled,
            "spawn_error" => Self::SpawnError,
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

impl Serialize for QueryWorkBashOutputResultAlt1Status {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QueryWorkBashOutputResultAlt1Status {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryWorkBashOutputResultAlt2 {
    #[serde(default = "fallback_query_work_bash_output_result_alt2_kind")]
    pub kind: QueryWorkBashOutputResultAlt2Kind,
    #[serde(rename = "workId")]
    pub work_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QueryWorkBashOutputResultAlt2Kind {
    Unavailable,
    Unsupported,
    ReadFailed,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QueryWorkBashOutputResultAlt2Kind {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unavailable;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Unsupported => "unsupported",
            Self::ReadFailed => "read_failed",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "unavailable" => Self::Unavailable,
            "unsupported" => Self::Unsupported,
            "read_failed" => Self::ReadFailed,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Unavailable,
            known => known.clone(),
        }
    }
}

impl Serialize for QueryWorkBashOutputResultAlt2Kind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QueryWorkBashOutputResultAlt2Kind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: String,
    pub v: u64,
    #[serde(default = "default_queue_item_kind")]
    pub kind: QueueItemKind,
    #[serde(default = "default_queue_item_text")]
    pub text: String,
    #[serde(
        rename = "sourceCommandId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_command_id: Option<String>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default = "default_queue_item_attachments")]
    pub attachments: Vec<Opaque>,
    #[serde(default = "default_queue_item_dispatch")]
    pub dispatch: QueueItemDispatch,
    #[serde(rename = "clientId", default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<QueueItemOrder>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<Opaque>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quotes: Option<Vec<InputQuote>>,
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
    #[serde(rename = "editLock", default, skip_serializing_if = "Option::is_none")]
    pub edit_lock: Option<QueueItemEditLock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QueueItemKind {
    Input,
    Goal,
    Compact,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QueueItemKind {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Input;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Input => "input",
            Self::Goal => "goal",
            Self::Compact => "compact",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "input" => Self::Input,
            "goal" => Self::Goal,
            "compact" => Self::Compact,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Input,
            known => known.clone(),
        }
    }
}

impl Serialize for QueueItemKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QueueItemKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueItemDispatch {
    #[serde(default = "default_queue_item_dispatch_state")]
    pub state: QueueItemDispatchState,
    #[serde(
        rename = "reservationId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reservation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QueueItemDispatchState {
    Waiting,
    Reserved,
    Dispatching,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QueueItemDispatchState {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Waiting;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Waiting => "waiting",
            Self::Reserved => "reserved",
            Self::Dispatching => "dispatching",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "waiting" => Self::Waiting,
            "reserved" => Self::Reserved,
            "dispatching" => Self::Dispatching,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Waiting,
            known => known.clone(),
        }
    }
}

impl Serialize for QueueItemDispatchState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QueueItemDispatchState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueItemOrder {
    #[serde(
        rename = "admissionSeq",
        default = "default_queue_item_order_admission_seq"
    )]
    pub admission_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueItemEditLock {
    #[serde(rename = "editorId")]
    pub editor_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestVerificationEntry {
    pub id: String,
    pub v: u64,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub provider: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub challenge: Opaque,
    #[serde(rename = "expiresAt", default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostAutomationCheckParams {
    #[serde(rename = "targetTaskId")]
    pub target_task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostAutomationCheckResult {
    pub bound: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostAutomationCreateResult {
    #[serde(rename = "automationId")]
    pub automation_id: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation: Option<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostAutomationDeleteParams {
    #[serde(rename = "automationId")]
    pub automation_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostAutomationDeleteResult {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostAutomationListResult {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub automations: Vec<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostAutomationUpdateParams {
    #[serde(rename = "automationId")]
    pub automation_id: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(flatten)]
    pub rest: BTreeMap<String, Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostAutomationUpdateResult {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation: Option<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostBrowserExecuteParams {
    #[serde(rename = "browserId")]
    pub browser_id: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub command: Opaque,
    #[serde(
        rename = "browserGeneration",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub browser_generation: Option<u64>,
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(rename = "turnId", default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(rename = "requestId", default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
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
    #[serde(
        rename = "remoteSessionId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub remote_session_id: Option<String>,
    #[serde(
        rename = "sessionContext",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub session_context: Option<ServiceHostBrowserExecuteParamsSessionContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ServiceHostBrowserExecuteParamsSessionContext {
    Live,
    Cached,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl ServiceHostBrowserExecuteParamsSessionContext {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Live;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Live => "live",
            Self::Cached => "cached",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "live" => Self::Live,
            "cached" => Self::Cached,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Live,
            known => known.clone(),
        }
    }
}

impl Serialize for ServiceHostBrowserExecuteParamsSessionContext {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ServiceHostBrowserExecuteParamsSessionContext {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostBrowserListParams {
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(rename = "turnId", default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(rename = "requestId", default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
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
    #[serde(
        rename = "remoteSessionId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub remote_session_id: Option<String>,
    #[serde(
        rename = "sessionContext",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub session_context: Option<ServiceHostBrowserExecuteParamsSessionContext>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostBrowserListResult {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub browsers: Vec<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostCredentialsCipherParams {
    pub operation: ServiceHostCredentialsCipherParamsOperation,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceHostCredentialsCipherParamsOperation {
    #[serde(rename = "encrypt")]
    Encrypt,
    #[serde(rename = "decrypt")]
    Decrypt,
}

impl ServiceHostCredentialsCipherParamsOperation {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Encrypt => "encrypt",
            Self::Decrypt => "decrypt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ServiceHostCredentialsCipherResult {
    #[serde(rename = "done")]
    Done(ServiceHostCredentialsCipherResultDone),
    #[serde(rename = "failed")]
    Failed(ServiceHostCredentialsCipherResultFailed),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostCredentialsCipherResultDone {
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHostCredentialsCipherResultFailed {
    #[serde(default = "fallback_service_host_credentials_cipher_result_failed_reason")]
    pub reason: ServiceHostCredentialsCipherResultFailedReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ServiceHostCredentialsCipherResultFailedReason {
    Decrypt,
    Unavailable,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl ServiceHostCredentialsCipherResultFailedReason {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unavailable;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Decrypt => "decrypt",
            Self::Unavailable => "unavailable",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "decrypt" => Self::Decrypt,
            "unavailable" => Self::Unavailable,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Unavailable,
            known => known.clone(),
        }
    }
}

impl Serialize for ServiceHostCredentialsCipherResultFailedReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ServiceHostCredentialsCipherResultFailedReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}
