//! 排版之前的摘要：73301d2037740be0
//! 生成物，不要手改：`catalog.plugins@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::PluginMarketplaceFieldsRefreshFailure;
use super::defaults::{
    default_command_plugins_configure_clear, default_command_plugins_configure_set,
    default_command_result_plugins_configure_diagnostics,
    default_plugin_catalog_entry_fields_builtin,
    default_plugin_catalog_entry_fields_component_types, default_plugin_fields_components,
    default_plugin_fields_components_item_items, default_plugin_fields_hooks,
    default_plugin_fields_hooks_item_runnable, default_plugin_fields_hooks_item_source_path,
    default_plugin_fields_mcp_server_names, default_plugin_fields_user,
    default_plugin_fields_user_config, default_plugin_layer_options,
    default_plugin_layer_secret_keys, default_plugin_marketplace_fields_featured,
    default_plugin_marketplace_fields_official, default_plugin_marketplace_fields_plugin_count,
    fallback_plugin_fields_components_item_kind, fallback_plugin_fields_hooks_item_type,
    fallback_plugin_fields_origin,
};
use crate::tolerant::Opaque;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPluginsCancel {
    #[serde(rename = "commandId")]
    pub command_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPluginsConfigure {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    pub scope: CommandPluginsConfigureScope,
    #[serde(default = "default_command_plugins_configure_set")]
    pub set: BTreeMap<String, CommandPluginsConfigureSetValue>,
    #[serde(default = "default_command_plugins_configure_clear")]
    pub clear: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandPluginsConfigureScope {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "workspace")]
    Workspace,
}

impl CommandPluginsConfigureScope {
    /// 线上的写法。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Workspace => "workspace",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandPluginsConfigureSetValue {
    Alt1(String),
    Alt2(f64),
    Alt3(bool),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPluginsInstall {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPluginsMarketplaceAdd {
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPluginsMarketplaceRemove {
    pub marketplace: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPluginsMarketplaceUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketplace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPluginsRescan {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPluginsSetEnabled {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    pub scope: CommandPluginsConfigureScope,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPluginsUninstall {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    #[serde(rename = "keepData", default, skip_serializing_if = "Option::is_none")]
    pub keep_data: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultPluginsCancel {
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultPluginsConfigure {
    #[serde(default = "default_command_result_plugins_configure_diagnostics")]
    pub diagnostics: Vec<PluginDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginDiagnostic {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<PluginDiagnosticSeverity>,
    #[serde(rename = "pluginId", default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PluginDiagnosticSeverity {
    Warning,
    Error,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl PluginDiagnosticSeverity {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Warning;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "warning" => Self::Warning,
            "error" => Self::Error,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Warning,
            known => known.clone(),
        }
    }
}

impl Serialize for PluginDiagnosticSeverity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PluginDiagnosticSeverity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultPluginsRescan {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginCatalogEntryFields {
    pub name: String,
    pub marketplace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(
        rename = "componentTypes",
        default = "default_plugin_catalog_entry_fields_component_types"
    )]
    pub component_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<PluginCatalogEntryFieldsListing>,
    pub installed: bool,
    #[serde(default = "default_plugin_catalog_entry_fields_builtin")]
    pub builtin: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginCatalogEntryFieldsListing {
    #[serde(
        rename = "displayName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name: Option<String>,
    #[serde(
        rename = "displayNameI18n",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name_i18n: Option<BTreeMap<String, String>>,
    #[serde(
        rename = "descriptionI18n",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub description_i18n: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(rename = "authorUrl", default, skip_serializing_if = "Option::is_none")]
    pub author_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(
        rename = "privacyPolicy",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub privacy_policy: Option<String>,
    #[serde(
        rename = "termsOfService",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub terms_of_service: Option<String>,
    #[serde(rename = "heroImage", default, skip_serializing_if = "Option::is_none")]
    pub hero_image: Option<String>,
    #[serde(
        rename = "examplePrompts",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub example_prompts: Option<Vec<String>>,
    #[serde(
        rename = "examplePromptsI18n",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub example_prompts_i18n: Option<BTreeMap<String, Vec<String>>>,
    #[serde(
        rename = "requiresPaidPlan",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub requires_paid_plan: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginFields {
    pub name: String,
    pub marketplace: String,
    #[serde(default = "fallback_plugin_fields_origin")]
    pub origin: PluginFieldsOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(rename = "authorUrl", default, skip_serializing_if = "Option::is_none")]
    pub author_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(rename = "rootPath")]
    pub root_path: String,
    #[serde(rename = "dataPath")]
    pub data_path: String,
    #[serde(default = "default_plugin_fields_components")]
    pub components: Vec<PluginFieldsComponentsItem>,
    #[serde(
        rename = "mcpServerNames",
        default = "default_plugin_fields_mcp_server_names"
    )]
    pub mcp_server_names: Vec<String>,
    #[serde(default = "default_plugin_fields_hooks")]
    pub hooks: Vec<PluginFieldsHooksItem>,
    #[serde(rename = "userConfig", default = "default_plugin_fields_user_config")]
    pub user_config: BTreeMap<String, PluginFieldsUserConfigValue>,
    #[serde(
        rename = "installedAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub installed_at: Option<String>,
    #[serde(
        rename = "latestVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub latest_version: Option<String>,
    #[serde(rename = "defaultEnabled")]
    pub default_enabled: bool,
    #[serde(default = "default_plugin_fields_user")]
    pub user: PluginLayer,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PluginFieldsOrigin {
    Marketplace,
    Builtin,
    Directory,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl PluginFieldsOrigin {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Marketplace;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Marketplace => "marketplace",
            Self::Builtin => "builtin",
            Self::Directory => "directory",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "marketplace" => Self::Marketplace,
            "builtin" => Self::Builtin,
            "directory" => Self::Directory,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Marketplace,
            known => known.clone(),
        }
    }
}

impl Serialize for PluginFieldsOrigin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PluginFieldsOrigin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginFieldsComponentsItem {
    #[serde(default = "fallback_plugin_fields_components_item_kind")]
    pub kind: PluginFieldsComponentsItemKind,
    #[serde(default = "default_plugin_fields_components_item_items")]
    pub items: Vec<PluginFieldsComponentsItemItemsItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PluginFieldsComponentsItemKind {
    Agent,
    Command,
    Skill,
    Hook,
    Mcp,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl PluginFieldsComponentsItemKind {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Skill;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Agent => "agent",
            Self::Command => "command",
            Self::Skill => "skill",
            Self::Hook => "hook",
            Self::Mcp => "mcp",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "agent" => Self::Agent,
            "command" => Self::Command,
            "skill" => Self::Skill,
            "hook" => Self::Hook,
            "mcp" => Self::Mcp,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Skill,
            known => known.clone(),
        }
    }
}

impl Serialize for PluginFieldsComponentsItemKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PluginFieldsComponentsItemKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginFieldsComponentsItemItemsItem {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginFieldsHooksItem {
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    #[serde(default = "fallback_plugin_fields_hooks_item_type")]
    pub r#type: PluginFieldsHooksItemType,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#async: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<PluginFieldsHooksItemShell>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(rename = "timeoutMs", default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(
        rename = "statusMessage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub status_message: Option<String>,
    #[serde(
        rename = "sourcePath",
        default = "default_plugin_fields_hooks_item_source_path"
    )]
    pub source_path: String,
    #[serde(default = "default_plugin_fields_hooks_item_runnable")]
    pub runnable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PluginFieldsHooksItemType {
    Command,
    Process,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl PluginFieldsHooksItemType {
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

impl Serialize for PluginFieldsHooksItemType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PluginFieldsHooksItemType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginFieldsHooksItemShell {
    Alt1(bool),
    Alt2(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginFieldsUserConfigValue {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<PluginFieldsUserConfigValueDefault>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<PluginFieldsUserConfigValueType>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginFieldsUserConfigValueDefault {
    Alt1(String),
    Alt2(f64),
    Alt3(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PluginFieldsUserConfigValueType {
    String,
    Number,
    Boolean,
    Directory,
    File,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl PluginFieldsUserConfigValueType {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::String;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Directory => "directory",
            Self::File => "file",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "string" => Self::String,
            "number" => Self::Number,
            "boolean" => Self::Boolean,
            "directory" => Self::Directory,
            "file" => Self::File,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::String,
            known => known.clone(),
        }
    }
}

impl Serialize for PluginFieldsUserConfigValueType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PluginFieldsUserConfigValueType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default = "default_plugin_layer_options")]
    pub options: BTreeMap<String, PluginFieldsUserConfigValueDefault>,
    #[serde(rename = "secretKeys", default = "default_plugin_layer_secret_keys")]
    pub secret_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginMarketplaceFields {
    /// 不透明 JSON：契约里没有类型（docs/zcode-protocol-v5/11-contract-codegen.md §9）。
    pub source: BTreeMap<String, Opaque>,
    #[serde(default = "default_plugin_marketplace_fields_official")]
    pub official: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(
        rename = "lastUpdated",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_updated: Option<String>,
    #[serde(
        rename = "pluginCount",
        default = "default_plugin_marketplace_fields_plugin_count"
    )]
    pub plugin_count: u64,
    #[serde(default = "default_plugin_marketplace_fields_featured")]
    pub featured: Vec<String>,
    #[serde(
        rename = "refreshFailure",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub refresh_failure: Option<PluginMarketplaceFieldsRefreshFailure>,
}
