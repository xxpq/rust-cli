//! 排版之前的摘要：2e066bc19f01082e
//! 生成物，不要手改：`catalog.plugins@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::defaults::{
    default_plugin_workspace_fields_workspace, default_query_plugins_describe_result_components,
    default_query_plugins_describe_result_diagnostics, fallback_plugin_operation_fields_kind,
    fallback_plugin_operation_fields_phase,
};
use super::{
    CommandPluginsInstall, CommandResultPluginsConfigure, PluginDiagnostic,
    PluginFieldsComponentsItem, PluginFieldsUserConfigValueDefault, PluginLayer,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginMarketplaceFieldsRefreshFailure {
    pub code: String,
    pub message: String,
    #[serde(rename = "failedAt")]
    pub failed_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginOperationFields {
    #[serde(default = "fallback_plugin_operation_fields_kind")]
    pub kind: PluginOperationFieldsKind,
    #[serde(rename = "pluginId", default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketplace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<PluginOperationFieldsScope>,
    #[serde(default = "fallback_plugin_operation_fields_phase")]
    pub phase: PluginOperationFieldsPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PluginOperationFieldsError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PluginOperationFieldsKind {
    Install,
    Uninstall,
    Update,
    SetEnabled,
    Configure,
    MarketplaceAdd,
    MarketplaceRemove,
    MarketplaceUpdate,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl PluginOperationFieldsKind {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Install;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Install => "install",
            Self::Uninstall => "uninstall",
            Self::Update => "update",
            Self::SetEnabled => "setEnabled",
            Self::Configure => "configure",
            Self::MarketplaceAdd => "marketplace.add",
            Self::MarketplaceRemove => "marketplace.remove",
            Self::MarketplaceUpdate => "marketplace.update",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "install" => Self::Install,
            "uninstall" => Self::Uninstall,
            "update" => Self::Update,
            "setEnabled" => Self::SetEnabled,
            "configure" => Self::Configure,
            "marketplace.add" => Self::MarketplaceAdd,
            "marketplace.remove" => Self::MarketplaceRemove,
            "marketplace.update" => Self::MarketplaceUpdate,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Install,
            known => known.clone(),
        }
    }
}

impl Serialize for PluginOperationFieldsKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PluginOperationFieldsKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PluginOperationFieldsScope {
    User,
    Workspace,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl PluginOperationFieldsScope {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::User;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::User => "user",
            Self::Workspace => "workspace",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "user" => Self::User,
            "workspace" => Self::Workspace,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::User,
            known => known.clone(),
        }
    }
}

impl Serialize for PluginOperationFieldsScope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PluginOperationFieldsScope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PluginOperationFieldsPhase {
    Waiting,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl PluginOperationFieldsPhase {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Running;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Waiting => "waiting",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "waiting" => Self::Waiting,
            "running" => Self::Running,
            "cancelling" => Self::Cancelling,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
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

impl Serialize for PluginOperationFieldsPhase {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PluginOperationFieldsPhase {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginOperationFieldsError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginWorkspaceFields {
    #[serde(default = "default_plugin_workspace_fields_workspace")]
    pub workspace: PluginLayer,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryPluginsDescribeParams {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryPluginsDescribeResult {
    #[serde(default = "default_query_plugins_describe_result_components")]
    pub components: Vec<PluginFieldsComponentsItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<QueryPluginsDescribeResultMetadata>,
    #[serde(default = "default_query_plugins_describe_result_diagnostics")]
    pub diagnostics: Vec<PluginDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryPluginsDescribeResultMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(rename = "authorUrl", default, skip_serializing_if = "Option::is_none")]
    pub author_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

pub type CommandPluginsUpdate = CommandPluginsInstall;

pub type CommandResultPluginsInstall = CommandResultPluginsConfigure;

pub type CommandResultPluginsMarketplaceAdd = CommandResultPluginsConfigure;

pub type CommandResultPluginsMarketplaceRemove = CommandResultPluginsConfigure;

pub type CommandResultPluginsMarketplaceUpdate = CommandResultPluginsConfigure;

pub type CommandResultPluginsSetEnabled = CommandResultPluginsConfigure;

pub type CommandResultPluginsUninstall = CommandResultPluginsConfigure;

pub type CommandResultPluginsUpdate = CommandResultPluginsConfigure;

pub type PluginOptionValue = PluginFieldsUserConfigValueDefault;

pub type PluginsState = CommandResultPluginsConfigure;
