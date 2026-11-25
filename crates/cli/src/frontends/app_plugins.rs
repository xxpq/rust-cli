//! protocol-v5 的插件管理端口的实现（docs/zcode-protocol-v5/24-plugins.md §13；protocol-v5.md §72.7）：cli 的插件管理服务
//! （`plugin_admin::PluginAdmin`，不知道 v5）的结果换成契约生成的类型，v5 的命令换成服务的操作。只换形状：这里不存插件的任何事实，
//! 每次调用都经服务读磁盘。写操作提交之后、返回之前拿走受影响的工作区内核，新会话用新的插件（24 §13.2）。
//!
//! 进程级部件（出口、内核缓存）在组装完成之后才交来（[`AppPlugins::ready`]）；握手不等组装，能力在握手时就声明。之前的读回空的事实加
//! 一条诊断，写回失败（24 §13.1）。

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use rust_cli_plugin_store::core::catalog::Listing;
use rust_cli_plugin_store::core::diagnostic::{Diagnostic, StoreError};
use rust_cli_plugin_store::core::id::{PluginId, is_official_marketplace};
use rust_cli_protocol_v5::generated::catalog_plugins_v1::{
    CommandPluginsConfigureScope, CommandPluginsConfigureSetValue, PluginCatalogEntryFields,
    PluginCatalogEntryFieldsListing, PluginDiagnostic, PluginFields, PluginFieldsComponentsItem,
    PluginFieldsComponentsItemItemsItem, PluginFieldsComponentsItemKind, PluginFieldsOrigin,
    PluginLayer, PluginMarketplaceFields, PluginMarketplaceFieldsRefreshFailure,
    PluginWorkspaceFields, QueryPluginsDescribeResult, QueryPluginsDescribeResultMetadata,
};
use rust_cli_protocol_v5::{
    PluginApplied, PluginInventory, PluginOp, PluginRefusal, WorkspacePlugins,
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::frontends::app_workspaces::{KernelCache, PluginsChanged};
use crate::plugin_admin::{
    ComponentKind, Control, Layer, Origin, Package, PluginAdmin, Scope, output,
};

/// 组装完成之前的诊断码与说明（24 §13.1）。
const NOT_READY_CODE: &str = "plugin_runtime_unavailable";
const NOT_READY: &str =
    "the plugin manager is not ready: the process-level parts are still assembling or failed";

/// 组装完成之后交来的部件。
struct Ready {
    admin: PluginAdmin,
    kernels: Arc<KernelCache>,
}

/// 插件管理的端口（`Settings.plugins`）。
#[derive(Default)]
pub(crate) struct AppPlugins {
    ready: OnceLock<Ready>,
}

impl AppPlugins {
    /// 组装完成：服务（用进程的同一个出口）与工作区内核的缓存。只交一次。
    pub(crate) fn ready(&self, admin: PluginAdmin, kernels: Arc<KernelCache>) {
        let _ = self.ready.set(Ready { admin, kernels });
    }
}

fn not_ready() -> Vec<PluginDiagnostic> {
    vec![PluginDiagnostic {
        code: NOT_READY_CODE.to_owned(),
        message: NOT_READY.to_owned(),
        severity: None,
        plugin_id: None,
    }]
}

/// 服务里的值经 JSON 换成生成的类型（hook 明细、userConfig、诊断：服务的 JSON 形状就是 Node 的形状，契约照它定）。读不通的一项丢掉。
fn wire<T: DeserializeOwned>(value: Value) -> Option<T> {
    serde_json::from_value(value).ok()
}

fn diagnostics(found: &[Diagnostic]) -> Vec<PluginDiagnostic> {
    found
        .iter()
        .filter_map(|diagnostic| wire(diagnostic.to_json()))
        .collect()
}

/// 一层的事实（24 §3.1）。
fn layer(found: &Layer) -> PluginLayer {
    PluginLayer {
        enabled: found.enabled,
        options: found
            .options
            .iter()
            .filter_map(|(key, value)| Some((key.clone(), wire(value.clone())?)))
            .collect(),
        secret_keys: found.secret_keys.clone(),
    }
}

fn components(found: &[(ComponentKind, Vec<String>)]) -> Vec<PluginFieldsComponentsItem> {
    found
        .iter()
        .map(|(kind, names)| PluginFieldsComponentsItem {
            kind: match kind {
                ComponentKind::Agent => PluginFieldsComponentsItemKind::Agent,
                ComponentKind::Command => PluginFieldsComponentsItemKind::Command,
                ComponentKind::Skill => PluginFieldsComponentsItemKind::Skill,
                ComponentKind::Hook => PluginFieldsComponentsItemKind::Hook,
                ComponentKind::Mcp => PluginFieldsComponentsItemKind::Mcp,
            },
            items: names
                .iter()
                .map(|name| PluginFieldsComponentsItemItemsItem {
                    name: name.clone(),
                    description: None,
                })
                .collect(),
        })
        .collect()
}

/// 用户级的一个插件（24 §3.2）。
fn plugin(package: &Package) -> PluginFields {
    let (author, author_url) = package.author();
    PluginFields {
        name: package.name.clone(),
        marketplace: package.marketplace.clone(),
        origin: match package.origin {
            Origin::Marketplace => PluginFieldsOrigin::Marketplace,
            Origin::Builtin => PluginFieldsOrigin::Builtin,
            Origin::Directory => PluginFieldsOrigin::Directory,
        },
        version: package.text("version"),
        description: package.text("description"),
        author,
        author_url,
        homepage: package.text("homepage"),
        root_path: package.root.clone(),
        data_path: package.data_path.clone(),
        components: components(&package.components),
        mcp_server_names: package.mcp_servers.clone(),
        hooks: package
            .hooks
            .iter()
            .filter_map(|hook| wire(output::hook_json(hook, &package.root)))
            .collect(),
        user_config: package
            .manifest
            .get("userConfig")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(key, option)| Some((key.clone(), wire(option.clone())?)))
            .collect(),
        installed_at: package.installed_at.clone(),
        latest_version: package.latest_version.clone(),
        default_enabled: package.default_enabled,
        user: layer(&package.user),
        enabled: package.enabled,
    }
}

fn listing(found: &Listing) -> PluginCatalogEntryFieldsListing {
    let map = |values: &BTreeMap<String, String>| (!values.is_empty()).then(|| values.clone());
    PluginCatalogEntryFieldsListing {
        display_name: found.display_name.clone(),
        display_name_i18n: map(&found.display_name_i18n),
        description_i18n: map(&found.description_i18n),
        icon: found.icon.clone(),
        category: found.category.clone(),
        author: found.author.clone(),
        author_url: found.author_url.clone(),
        homepage: found.homepage.clone(),
        privacy_policy: found.privacy_policy.clone(),
        terms_of_service: found.terms_of_service.clone(),
        hero_image: found.hero_image.clone(),
        example_prompts: (!found.example_prompts.is_empty()).then(|| found.example_prompts.clone()),
        example_prompts_i18n: (!found.example_prompts_i18n.is_empty())
            .then(|| found.example_prompts_i18n.clone()),
        requires_paid_plan: found.requires_paid_plan.then_some(true),
    }
}

/// 服务的错误 → 拒绝（24 §4.1）。
fn refusal(error: StoreError) -> PluginRefusal {
    match error {
        StoreError::PluginNotFound(id) => PluginRefusal::PluginNotFound(id),
        StoreError::MarketplaceNotFound(name) => PluginRefusal::MarketplaceNotFound(name),
        StoreError::Cancelled => PluginRefusal::Cancelled,
        other => PluginRefusal::Failed(other.to_string()),
    }
}

fn plugin_id(text: &str) -> Result<PluginId, PluginRefusal> {
    PluginId::parse(text).ok_or_else(|| PluginRefusal::PluginNotFound(text.to_owned()))
}

/// 配置值的检查（24 §4.1 的 `pluginOptionInvalid`）：键要在 manifest 的 `userConfig` 里，值的类型对得上（`number` 是数、`boolean` 是布尔，
/// 别的是字符串）。服务照 Node 不检查，前端在这里挡（plugin-store.md §12.1）。
fn check_options(
    user_config: Option<&Value>,
    set: &BTreeMap<String, CommandPluginsConfigureSetValue>,
    clear: &[String],
) -> Result<(), PluginRefusal> {
    let declared = user_config.and_then(Value::as_object);
    let option = |key: &str| declared.and_then(|declared| declared.get(key));
    for key in set.keys().chain(clear) {
        if option(key).is_none() {
            return Err(PluginRefusal::OptionInvalid {
                key: key.clone(),
                message: format!("{key} is not a configuration option of this plugin"),
            });
        }
    }
    for (key, value) in set {
        let wanted = option(key)
            .and_then(|option| option.get("type"))
            .and_then(Value::as_str)
            .unwrap_or(STRING_TYPE);
        let fits = match value {
            CommandPluginsConfigureSetValue::Alt1(_) => {
                !matches!(wanted, NUMBER_TYPE | BOOLEAN_TYPE)
            }
            CommandPluginsConfigureSetValue::Alt2(_) => wanted == NUMBER_TYPE,
            CommandPluginsConfigureSetValue::Alt3(_) => wanted == BOOLEAN_TYPE,
        };
        if !fits {
            return Err(PluginRefusal::OptionInvalid {
                key: key.clone(),
                message: format!("{key} expects a {wanted} value"),
            });
        }
    }
    Ok(())
}

/// `userConfig` 里的值类型（Node `PluginUserConfigOption.type`）。
const STRING_TYPE: &str = "string";
const NUMBER_TYPE: &str = "number";
const BOOLEAN_TYPE: &str = "boolean";

/// 设值的参数换成服务收的 JSON 值。
fn set_values(set: &BTreeMap<String, CommandPluginsConfigureSetValue>) -> Map<String, Value> {
    set.iter()
        .map(|(key, value)| {
            let value = match value {
                CommandPluginsConfigureSetValue::Alt1(text) => Value::String(text.clone()),
                CommandPluginsConfigureSetValue::Alt2(number) => {
                    serde_json::Number::from_f64(*number).map_or(Value::Null, Value::Number)
                }
                CommandPluginsConfigureSetValue::Alt3(flag) => Value::Bool(*flag),
            };
            (key.clone(), value)
        })
        .collect()
}

/// 写到哪一层：`scope: workspace` 时是命令的工作区（端口给的），否则是用户层（以 home 为工作区读，24 §13.1）。
fn scope<'a>(
    scope: CommandPluginsConfigureScope,
    workspace: Option<&'a str>,
    home: &'a str,
) -> (Scope, &'a str) {
    match (scope, workspace) {
        (CommandPluginsConfigureScope::Workspace, Some(root)) => (Scope::Workspace, root),
        _ => (Scope::User, home),
    }
}

impl Ready {
    /// 一条写：服务的操作；返回结论与哪一层改了（市场的三条、noop 没有）。
    async fn write(
        &self,
        op: PluginOp,
        control: &Control,
    ) -> (Result<PluginApplied, PluginRefusal>, Option<PluginsChanged>) {
        let home = self.admin.home();
        let applied = |noop: bool, found: &[Diagnostic]| PluginApplied {
            noop,
            diagnostics: diagnostics(found),
        };
        match op {
            PluginOp::Install(change) => {
                let id = match plugin_id(&change.plugin_id) {
                    Ok(id) => id,
                    Err(refused) => return (Err(refused), None),
                };
                let outcome = self.admin.install(&home, &id, control).await;
                match outcome.error {
                    Some(error) => (Err(refusal(error)), Some(PluginsChanged::User)),
                    None => (
                        Ok(applied(outcome.unchanged, &outcome.diagnostics)),
                        (!outcome.unchanged).then_some(PluginsChanged::User),
                    ),
                }
            }
            PluginOp::Update(change) => {
                match self.admin.update(&home, &change.plugin_id, control).await {
                    Ok((outcome, _)) => match outcome.error {
                        Some(error) => (Err(refusal(error)), Some(PluginsChanged::User)),
                        None => (
                            Ok(applied(outcome.unchanged, &outcome.diagnostics)),
                            (!outcome.unchanged).then_some(PluginsChanged::User),
                        ),
                    },
                    Err(error) => (Err(refusal(error)), None),
                }
            }
            PluginOp::Uninstall(change) => {
                let keep = change.keep_data.unwrap_or(false);
                match self
                    .admin
                    .uninstall(&home, &change.plugin_id, keep, control)
                    .await
                {
                    Ok(removed) => (
                        Ok(applied(removed.is_none(), &[])),
                        removed.is_some().then_some(PluginsChanged::User),
                    ),
                    Err(error) => (Err(refusal(error)), Some(PluginsChanged::User)),
                }
            }
            PluginOp::SetEnabled { workspace, change } => {
                let (layer, cwd) = scope(change.scope, workspace.as_deref(), &home);
                let changed = changed_layer(layer, cwd);
                match self
                    .admin
                    .set_enabled(cwd, &change.plugin_id, layer, change.enabled, control)
                    .await
                {
                    Ok(done) => {
                        let changed = done.changed.then_some(changed);
                        (Ok(applied(!done.changed, &[])), changed)
                    }
                    Err(error) => (Err(refusal(error)), Some(changed)),
                }
            }
            PluginOp::Configure { workspace, change } => {
                let (layer, cwd) = scope(change.scope, workspace.as_deref(), &home);
                let changed = changed_layer(layer, cwd);
                let listing = self.admin.list(cwd).await;
                let user_config = match listing.select(&change.plugin_id) {
                    Ok(found) => found.user_config.clone(),
                    Err(error) => return (Err(refusal(error)), None),
                };
                if let Err(refused) =
                    check_options(user_config.as_ref(), &change.set, &change.clear)
                {
                    return (Err(refused), None);
                }
                let set = set_values(&change.set);
                match self
                    .admin
                    .configure(
                        cwd,
                        &change.plugin_id,
                        layer,
                        (&set, &change.clear),
                        control,
                    )
                    .await
                {
                    Ok(done) => (Ok(applied(!done, &[])), done.then_some(changed)),
                    Err(error) => (Err(refusal(error)), Some(changed)),
                }
            }
            // 市场的三条：加载不读市场，内核不受影响
            PluginOp::MarketplaceAdd(change) => (
                self.admin
                    .marketplace_add(&home, &change.source, &[], control)
                    .await
                    .map(|_| applied(false, &[]))
                    .map_err(refusal),
                None,
            ),
            PluginOp::MarketplaceRemove(change) => (
                self.admin
                    .marketplace_remove(&home, &change.marketplace, control)
                    .await
                    .map(|removed| applied(!removed, &[]))
                    .map_err(refusal),
                None,
            ),
            PluginOp::MarketplaceUpdate(change) => (
                self.admin
                    .marketplace_refresh(&home, change.marketplace.as_deref(), control)
                    .await
                    .map(|refreshed| applied(false, &refreshed.diagnostics))
                    .map_err(refusal),
                None,
            ),
        }
    }
}

/// 一层的改动影响哪些工作区：用户层是全部，工作区层是那一棵（24 §13.2）。
fn changed_layer(layer: Scope, cwd: &str) -> PluginsChanged {
    match layer {
        Scope::User => PluginsChanged::User,
        Scope::Workspace => PluginsChanged::Workspace(cwd.to_owned()),
    }
}

#[async_trait]
impl rust_cli_protocol_v5::PluginAdmin for AppPlugins {
    async fn inventory(&self) -> PluginInventory {
        let Some(ready) = self.ready.get() else {
            return PluginInventory {
                diagnostics: not_ready(),
                ..PluginInventory::default()
            };
        };
        let found = ready.admin.inventory(&ready.admin.home()).await;
        let builtin = |id: &str| {
            found
                .packages
                .iter()
                .any(|package| package.id == id && package.origin == Origin::Builtin)
        };
        let mut inventory = PluginInventory {
            plugins: found
                .packages
                .iter()
                .map(|package| (package.id.clone(), plugin(package)))
                .collect(),
            diagnostics: diagnostics(&found.diagnostics),
            ..PluginInventory::default()
        };
        for record in &found.catalog.marketplaces {
            inventory.marketplaces.insert(
                record.id.clone(),
                PluginMarketplaceFields {
                    source: record
                        .source
                        .raw
                        .as_object()
                        .map(|source| source.clone().into_iter().collect())
                        .unwrap_or_default(),
                    official: is_official_marketplace(&record.id),
                    description: record.description.clone(),
                    last_updated: record.last_updated.clone(),
                    plugin_count: record.plugin_count,
                    featured: found
                        .catalog
                        .featured
                        .get(&record.id)
                        .cloned()
                        .unwrap_or_default(),
                    refresh_failure: record
                        .refresh_failure
                        .clone()
                        .and_then(wire::<PluginMarketplaceFieldsRefreshFailure>),
                },
            );
        }
        // 可装的条目与隐藏了的内置（`builtin && !installed`，24 §3.4）
        for (entry, restorable) in found
            .catalog
            .available
            .iter()
            .map(|entry| (entry, false))
            .chain(found.catalog.restorable.iter().map(|entry| (entry, true)))
        {
            let builtin = restorable || builtin(&entry.id);
            inventory.catalog.insert(
                entry.id.clone(),
                PluginCatalogEntryFields {
                    name: entry.name.clone(),
                    marketplace: entry.marketplace.clone(),
                    description: entry.description.clone(),
                    version: entry.version.clone(),
                    component_types: entry.component_types.clone(),
                    listing: entry.listing.as_ref().map(listing),
                    // 内置的没有安装记录：没隐藏就算装着
                    installed: entry.installed || (builtin && !restorable),
                    builtin,
                },
            );
        }
        inventory
    }

    async fn workspace(&self, key: &str) -> WorkspacePlugins {
        let Some(ready) = self.ready.get() else {
            return WorkspacePlugins {
                diagnostics: not_ready(),
                ..WorkspacePlugins::default()
            };
        };
        let found = ready.admin.workspace(key).await;
        WorkspacePlugins {
            plugins: found
                .plugins
                .iter()
                .map(|(id, found, enabled)| {
                    (
                        id.clone(),
                        PluginWorkspaceFields {
                            workspace: layer(found),
                            enabled: *enabled,
                        },
                    )
                })
                .collect(),
            diagnostics: diagnostics(&found.diagnostics),
        }
    }

    async fn apply(
        &self,
        op: PluginOp,
        cancel: CancellationToken,
    ) -> Result<PluginApplied, PluginRefusal> {
        let Some(ready) = self.ready.get() else {
            return Err(PluginRefusal::Failed(NOT_READY.to_owned()));
        };
        let control = Control::new(cancel);
        let (result, changed) = ready.write(op, &control).await;
        // 提交点之前取消的什么也没变；别的失败可能在提交点之后，照样拿走（代价只是下一次重新组装）
        if let Some(changed) = changed.filter(|_| result != Err(PluginRefusal::Cancelled)) {
            let gone = ready.kernels.plugins_changed(&changed);
            tracing::info!(
                event = "app.workspace.plugins_changed",
                workspaces = gone.len(),
                "dropped the workspace kernels a plugin change affects; new sessions assemble with the new plugins"
            );
        }
        result
    }

    async fn describe(&self, plugin_id: &str) -> Result<QueryPluginsDescribeResult, PluginRefusal> {
        let Some(ready) = self.ready.get() else {
            return Err(PluginRefusal::Failed(NOT_READY.to_owned()));
        };
        let found = ready
            .admin
            .describe(&ready.admin.home(), plugin_id)
            .await
            .map_err(refusal)?;
        let text = |key: &str| {
            found
                .manifest
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let (author, author_url) =
            rust_cli_plugin_store::core::catalog::author(found.manifest.get("author"));
        let metadata = QueryPluginsDescribeResultMetadata {
            author,
            author_url,
            homepage: text("homepage"),
            description: text("description"),
            version: text("version"),
        };
        Ok(QueryPluginsDescribeResult {
            components: components(&found.components),
            metadata: found.manifest.is_object().then_some(metadata),
            diagnostics: diagnostics(&found.diagnostics),
        })
    }
}
