//! 排版之前的摘要：a89129255bbe52c3
//! 生成物，不要手改：`catalog.plugins@1` 的线上类型（docs/zcode-protocol-v5/11-contract-codegen.md）。
//! 重新生成：`node --import tsx scripts/rust-cli/protocol-v5/generate.mjs`。

use super::{
    CommandPluginsConfigureSetValue, PluginDiagnostic, PluginFieldsComponentsItem,
    PluginFieldsComponentsItemItemsItem, PluginFieldsComponentsItemKind, PluginFieldsHooksItem,
    PluginFieldsHooksItemType, PluginFieldsOrigin, PluginFieldsUserConfigValue,
    PluginFieldsUserConfigValueDefault, PluginLayer, PluginOperationFieldsKind,
    PluginOperationFieldsPhase,
};
use std::collections::BTreeMap;

pub(super) fn default_command_plugins_configure_set()
-> BTreeMap<String, CommandPluginsConfigureSetValue> {
    BTreeMap::new()
}

pub(super) fn default_command_plugins_configure_clear() -> Vec<String> {
    Vec::new()
}

pub(super) fn default_command_result_plugins_configure_diagnostics() -> Vec<PluginDiagnostic> {
    Vec::new()
}

pub(super) fn default_plugin_catalog_entry_fields_component_types() -> Vec<String> {
    Vec::new()
}

pub(super) fn default_plugin_catalog_entry_fields_builtin() -> bool {
    false
}

pub(super) fn fallback_plugin_fields_origin() -> PluginFieldsOrigin {
    PluginFieldsOrigin::Marketplace
}

pub(super) fn fallback_plugin_fields_components_item_kind() -> PluginFieldsComponentsItemKind {
    PluginFieldsComponentsItemKind::Skill
}

pub(super) fn default_plugin_fields_components_item_items()
-> Vec<PluginFieldsComponentsItemItemsItem> {
    Vec::new()
}

pub(super) fn default_plugin_fields_components() -> Vec<PluginFieldsComponentsItem> {
    Vec::new()
}

pub(super) fn default_plugin_fields_mcp_server_names() -> Vec<String> {
    Vec::new()
}

pub(super) fn fallback_plugin_fields_hooks_item_type() -> PluginFieldsHooksItemType {
    PluginFieldsHooksItemType::Command
}

pub(super) fn default_plugin_fields_hooks_item_source_path() -> String {
    String::new()
}

pub(super) fn default_plugin_fields_hooks_item_runnable() -> bool {
    false
}

pub(super) fn default_plugin_fields_hooks() -> Vec<PluginFieldsHooksItem> {
    Vec::new()
}

pub(super) fn default_plugin_fields_user_config() -> BTreeMap<String, PluginFieldsUserConfigValue> {
    BTreeMap::new()
}

pub(super) fn default_plugin_layer_options() -> BTreeMap<String, PluginFieldsUserConfigValueDefault>
{
    BTreeMap::new()
}

pub(super) fn default_plugin_layer_secret_keys() -> Vec<String> {
    Vec::new()
}

pub(super) fn default_plugin_fields_user() -> PluginLayer {
    PluginLayer {
        enabled: None,
        options: default_plugin_layer_options(),
        secret_keys: default_plugin_layer_secret_keys(),
    }
}

pub(super) fn default_plugin_marketplace_fields_official() -> bool {
    false
}

pub(super) fn default_plugin_marketplace_fields_plugin_count() -> u64 {
    0
}

pub(super) fn default_plugin_marketplace_fields_featured() -> Vec<String> {
    Vec::new()
}

pub(super) fn fallback_plugin_operation_fields_kind() -> PluginOperationFieldsKind {
    PluginOperationFieldsKind::Install
}

pub(super) fn fallback_plugin_operation_fields_phase() -> PluginOperationFieldsPhase {
    PluginOperationFieldsPhase::Running
}

pub(super) fn default_plugin_workspace_fields_workspace() -> PluginLayer {
    PluginLayer {
        enabled: None,
        options: default_plugin_layer_options(),
        secret_keys: default_plugin_layer_secret_keys(),
    }
}

pub(super) fn default_query_plugins_describe_result_components() -> Vec<PluginFieldsComponentsItem>
{
    Vec::new()
}

pub(super) fn default_query_plugins_describe_result_diagnostics() -> Vec<PluginDiagnostic> {
    Vec::new()
}
