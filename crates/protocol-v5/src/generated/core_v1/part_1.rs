//! 排版之前的摘要：c0084b16705b261f
//! 生成物，不要手改：`core@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults::{
    default_command_envelope_payload, default_command_result_accepted_effects,
    default_command_result_accepted_noop, default_credit_n, default_initialize_params_peer,
    default_initialize_params_peer_build, default_initialize_params_peer_build_version,
    default_initialize_params_peer_kind, default_initialize_params_peer_name,
    default_initialize_params_require, default_initialize_rejection_missing_item_mine,
    default_initialize_rejection_missing_item_theirs, default_initialize_rejection_upgrade_message,
    default_initialize_result_server, default_snapshot_collections, default_snapshot_state,
    fallback_exit_reason, fallback_initialize_rejection_upgrade_target,
    fallback_initialize_result_delivery, fallback_initialize_result_principal,
};
use super::{InitializeResultLimits, InitializeResultServer};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    #[serde(rename = "commandId")]
    pub command_id: String,
    pub r#type: String,
    pub target: CommandTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Expect>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default = "default_command_envelope_payload")]
    pub payload: Opaque,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandTarget {
    #[serde(rename = "hostId")]
    pub host_id: String,
    #[serde(
        rename = "workspaceKey",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_key: Option<String>,
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Expect {
    Alt1(ExpectAlt1),
    Alt2(ExpectAlt2),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectAlt1 {
    pub col: String,
    pub id: String,
    pub v: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectAlt2 {
    pub col: String,
    pub id: String,
    pub field: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub value: Opaque,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CommandResult {
    #[serde(rename = "accepted")]
    Accepted(CommandResultAccepted),
    #[serde(rename = "rejected")]
    Rejected(CommandResultRejected),
    #[serde(rename = "duplicate")]
    Duplicate(CommandResultDuplicate),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultAccepted {
    #[serde(default = "default_command_result_accepted_effects")]
    pub effects: Vec<CommandResultAcceptedEffectsItem>,
    #[serde(default = "default_command_result_accepted_noop")]
    pub noop: bool,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultAcceptedEffectsItem {
    pub topic: String,
    pub epoch: String,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultRejected {
    pub code: String,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<Opaque>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultDuplicate {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub original: Opaque,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credit {
    pub sub: String,
    #[serde(default = "default_credit_n")]
    pub n: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    pub epoch: String,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum DeltaOp {
    #[serde(rename = "entity.put")]
    EntityPut(DeltaOpEntityPut),
    #[serde(rename = "entity.append")]
    EntityAppend(DeltaOpEntityAppend),
    #[serde(rename = "entity.remove")]
    EntityRemove(DeltaOpEntityRemove),
    #[serde(rename = "state.set")]
    StateSet(DeltaOpStateSet),
    #[serde(rename = "entity.patch")]
    EntityPatch(DeltaOpEntityPatch),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaOpEntityPut {
    pub col: String,
    pub id: String,
    pub v: u64,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub data: BTreeMap<String, Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaOpEntityAppend {
    pub col: String,
    pub id: String,
    pub path: String,
    pub chunk: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaOpEntityRemove {
    pub col: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaOpStateSet {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub patch: BTreeMap<String, Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaOpEntityPatch {
    pub col: String,
    pub id: String,
    pub v: u64,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub fields: BTreeMap<String, Opaque>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unset: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exit {
    #[serde(default = "fallback_exit_reason")]
    pub reason: ExitReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExitReason {
    ClientClose,
    ServerShutdown,
    Replaced,
    ProtocolViolation,
    Unknown,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl ExitReason {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unknown;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::ClientClose => "client_close",
            Self::ServerShutdown => "server_shutdown",
            Self::Replaced => "replaced",
            Self::ProtocolViolation => "protocol_violation",
            Self::Unknown => "unknown",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "client_close" => Self::ClientClose,
            "server_shutdown" => Self::ServerShutdown,
            "replaced" => Self::Replaced,
            "protocol_violation" => Self::ProtocolViolation,
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

impl Serialize for ExitReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ExitReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub topic: String,
    pub sub: String,
    pub epoch: String,
    pub from: u64,
    pub to: u64,
    pub body: FrameBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum FrameBody {
    #[serde(rename = "snapshot")]
    Snapshot(FrameBodySnapshot),
    #[serde(rename = "deltas")]
    Deltas(FrameBodyDeltas),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameBodySnapshot {
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default = "default_snapshot_state")]
    pub state: BTreeMap<String, Opaque>,
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    #[serde(default = "default_snapshot_collections")]
    pub collections: BTreeMap<String, Vec<Opaque>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameBodyDeltas {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub deltas: Vec<Opaque>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "clientId")]
    pub client_id: String,
    #[serde(default = "default_initialize_params_peer")]
    pub peer: InitializeParamsPeer,
    pub caps: BTreeMap<String, Vec<u64>>,
    #[serde(default = "default_initialize_params_require")]
    pub require: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,
    #[serde(
        rename = "hostServices",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub host_services: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeParamsPeer {
    #[serde(default = "default_initialize_params_peer_kind")]
    pub kind: InitializeParamsPeerKind,
    #[serde(default = "default_initialize_params_peer_name")]
    pub name: String,
    #[serde(default = "default_initialize_params_peer_build")]
    pub build: InitializeParamsPeerBuild,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InitializeParamsPeerKind {
    DesktopWindow,
    WebPage,
    MobileH5,
    Bot,
    Host,
    Cli,
    Unknown,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl InitializeParamsPeerKind {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unknown;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::DesktopWindow => "desktop-window",
            Self::WebPage => "web-page",
            Self::MobileH5 => "mobile-h5",
            Self::Bot => "bot",
            Self::Host => "host",
            Self::Cli => "cli",
            Self::Unknown => "unknown",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "desktop-window" => Self::DesktopWindow,
            "web-page" => Self::WebPage,
            "mobile-h5" => Self::MobileH5,
            "bot" => Self::Bot,
            "host" => Self::Host,
            "cli" => Self::Cli,
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

impl Serialize for InitializeParamsPeerKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InitializeParamsPeerKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeParamsPeerBuild {
    #[serde(default = "default_initialize_params_peer_build_version")]
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeRejection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<Vec<InitializeRejectionMissingItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade: Option<InitializeRejectionUpgrade>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<InitializeRejectionContract>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeRejectionMissingItem {
    pub cap: String,
    #[serde(default = "default_initialize_rejection_missing_item_mine")]
    pub mine: Vec<i64>,
    #[serde(default = "default_initialize_rejection_missing_item_theirs")]
    pub theirs: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeRejectionUpgrade {
    #[serde(default = "fallback_initialize_rejection_upgrade_target")]
    pub target: InitializeRejectionUpgradeTarget,
    #[serde(default = "default_initialize_rejection_upgrade_message")]
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InitializeRejectionUpgradeTarget {
    Client,
    Server,
    Both,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl InitializeRejectionUpgradeTarget {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Both;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
            Self::Both => "both",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "client" => Self::Client,
            "server" => Self::Server,
            "both" => Self::Both,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Both,
            known => known.clone(),
        }
    }
}

impl Serialize for InitializeRejectionUpgradeTarget {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InitializeRejectionUpgradeTarget {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeRejectionContract {
    pub client: String,
    pub server: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "connectionId")]
    pub connection_id: String,
    #[serde(default = "fallback_initialize_result_principal")]
    pub principal: InitializeResultPrincipal,
    #[serde(default = "fallback_initialize_result_delivery")]
    pub delivery: InitializeResultDelivery,
    pub caps: BTreeMap<String, u64>,
    pub limits: InitializeResultLimits,
    #[serde(default = "default_initialize_result_server")]
    pub server: InitializeResultServer,
    pub contract: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InitializeResultPrincipal {
    LocalWindow,
    WebPage,
    PairedMobile,
    Bot,
    UpstreamHost,
    Unknown,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl InitializeResultPrincipal {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Unknown;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::LocalWindow => "local-window",
            Self::WebPage => "web-page",
            Self::PairedMobile => "paired-mobile",
            Self::Bot => "bot",
            Self::UpstreamHost => "upstream-host",
            Self::Unknown => "unknown",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "local-window" => Self::LocalWindow,
            "web-page" => Self::WebPage,
            "paired-mobile" => Self::PairedMobile,
            "bot" => Self::Bot,
            "upstream-host" => Self::UpstreamHost,
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

impl Serialize for InitializeResultPrincipal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InitializeResultPrincipal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InitializeResultDelivery {
    Continuous,
    Replayable,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl InitializeResultDelivery {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Replayable;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Continuous => "continuous",
            Self::Replayable => "replayable",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "continuous" => Self::Continuous,
            "replayable" => Self::Replayable,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Replayable,
            known => known.clone(),
        }
    }
}

impl Serialize for InitializeResultDelivery {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InitializeResultDelivery {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}
