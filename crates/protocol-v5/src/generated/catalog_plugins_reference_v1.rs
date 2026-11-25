//! 排版之前的摘要：70ede6282dd7cce9
//! 生成物，不要手改：`catalog.plugins.reference@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryPluginsReferenceCatalogParams {
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryPluginsReferenceCatalogResult {
    #[serde(default = "fallback_query_plugins_reference_catalog_result_authority")]
    pub authority: QueryPluginsReferenceCatalogResultAuthority,
    #[serde(default = "default_query_plugins_reference_catalog_result_plugins")]
    pub plugins: Vec<QueryPluginsReferenceCatalogResultPluginsItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QueryPluginsReferenceCatalogResultAuthority {
    Session,
    Workspace,
    /// 不认识的值（对端更新）：原文保留，写出时原样写回。
    Unrecognized(String),
}

impl QueryPluginsReferenceCatalogResultAuthority {
    /// `.catch` 的值。
    pub const FALLBACK: Self = Self::Workspace;

    /// 线上的写法（不认识的值是原文）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Session => "session",
            Self::Workspace => "workspace",
            Self::Unrecognized(text) => text,
        }
    }

    /// 线上的写法 → 值；不认识时是 `Unrecognized`。
    pub fn from_wire(text: &str) -> Self {
        match text {
            "session" => Self::Session,
            "workspace" => Self::Workspace,
            other => Self::Unrecognized(other.to_owned()),
        }
    }

    /// 读端宽松（与 `.catch` 相同）：不认识的值降级成 [`Self::FALLBACK`]。
    pub fn resolved(&self) -> Self {
        match self {
            Self::Unrecognized(_) => Self::Workspace,
            known => known.clone(),
        }
    }
}

impl Serialize for QueryPluginsReferenceCatalogResultAuthority {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QueryPluginsReferenceCatalogResultAuthority {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(&crate::tolerant::enum_text(deserializer)?))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryPluginsReferenceCatalogResultPluginsItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    pub name: String,
    pub marketplace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(
        rename = "descriptionI18n",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub description_i18n: Option<BTreeMap<String, String>>,
    pub enabled: bool,
    #[serde(
        rename = "conflictingPluginIds",
        default = "default_query_plugins_reference_catalog_result_plugins_item_conflicting_plugin_ids"
    )]
    pub conflicting_plugin_ids: Vec<String>,
    #[serde(
        rename = "skillQualifiedNames",
        default = "default_query_plugins_reference_catalog_result_plugins_item_skill_qualified_names"
    )]
    pub skill_qualified_names: Vec<String>,
    #[serde(
        rename = "mcpServerNames",
        default = "default_query_plugins_reference_catalog_result_plugins_item_mcp_server_names"
    )]
    pub mcp_server_names: Vec<String>,
    #[serde(
        rename = "subagentNames",
        default = "default_query_plugins_reference_catalog_result_plugins_item_subagent_names"
    )]
    pub subagent_names: Vec<String>,
}

pub type QueryPluginsReferenceCatalogWithCategoryParams = QueryPluginsReferenceCatalogParams;

pub type QueryPluginsReferenceCatalogWithCategoryResult = QueryPluginsReferenceCatalogResult;

fn fallback_query_plugins_reference_catalog_result_authority()
-> QueryPluginsReferenceCatalogResultAuthority {
    QueryPluginsReferenceCatalogResultAuthority::Workspace
}

fn default_query_plugins_reference_catalog_result_plugins_item_conflicting_plugin_ids()
-> Vec<String> {
    Vec::new()
}

fn default_query_plugins_reference_catalog_result_plugins_item_skill_qualified_names() -> Vec<String>
{
    Vec::new()
}

fn default_query_plugins_reference_catalog_result_plugins_item_mcp_server_names() -> Vec<String> {
    Vec::new()
}

fn default_query_plugins_reference_catalog_result_plugins_item_subagent_names() -> Vec<String> {
    Vec::new()
}

fn default_query_plugins_reference_catalog_result_plugins()
-> Vec<QueryPluginsReferenceCatalogResultPluginsItem> {
    Vec::new()
}
