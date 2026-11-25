//! 插件管理端口的替身（只给本 crate 的测试，protocol-v5.md §72.6）：一份内存里的「磁盘」当事实，`apply` 改它，`inventory` /
//! `workspace` 每次从它现算。能把写挡在提交点之前、把读挡在读完之后，等测试放行（不靠计时）。
//! 不是插件管理的实现：市场、安装、配置都只做到测试要的程度；真实现在 cli（`frontends/app_plugins.rs`）。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::sync::LockUnpoisoned;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::generated::catalog_plugins_v1::{
    CommandPluginsConfigureSetValue, PluginCatalogEntryFields, PluginFields,
    PluginFieldsComponentsItem, PluginFieldsComponentsItemItemsItem,
    PluginFieldsComponentsItemKind, PluginFieldsOrigin, PluginFieldsUserConfigValue, PluginLayer,
    PluginMarketplaceFields, PluginOptionValue, PluginWorkspaceFields, QueryPluginsDescribeResult,
};
use crate::plugin_admin::{
    PluginAdmin, PluginApplied, PluginInventory, PluginOp, PluginRefusal, WorkspacePlugins,
};

/// 一个可加的市场（「远端」：`marketplace.add` 的来源指到它）。
#[derive(Clone)]
pub(crate) struct Market {
    pub(crate) name: String,
    /// 插件名 → userConfig（键 → 是否敏感）。
    pub(crate) plugins: BTreeMap<String, BTreeMap<String, bool>>,
}

/// 一层的事实。
#[derive(Clone, Default)]
struct Layer {
    enabled: Option<bool>,
    options: BTreeMap<String, PluginOptionValue>,
    secrets: BTreeMap<String, String>,
}

#[derive(Default)]
struct Disk {
    /// 来源 → 市场（可以加的）。
    sources: BTreeMap<String, Market>,
    /// 已加的市场：名字 → 来源。
    added: BTreeMap<String, String>,
    installed: Vec<String>,
    user: BTreeMap<String, Layer>,
    workspaces: BTreeMap<String, BTreeMap<String, Layer>>,
}

/// 把读或写挡住的闸门：进了闸门报 `entered`，等 `release`。
#[derive(Default)]
pub(crate) struct Gate {
    pub(crate) entered: Notify,
    pub(crate) release: Notify,
}

#[derive(Default)]
pub(crate) struct FakeAdmin {
    disk: Mutex<Disk>,
    /// 挡要下载的写：在提交点之前等放行（能被取消）。
    pub(crate) write_gate: Mutex<Option<Arc<Gate>>>,
    /// 挡读：读完之后、交出结果之前等放行（模拟慢的读）。
    pub(crate) read_gate: Mutex<Option<Arc<Gate>>>,
    /// 调过端口的写（测试看「取消了的没有调端口」）。
    pub(crate) applied: Mutex<Vec<PluginOp>>,
}

fn id(name: &str, market: &str) -> String {
    format!("{name}@{market}")
}

impl FakeAdmin {
    pub(crate) fn with_source(source: &str, market: Market) -> Self {
        let admin = Self::default();
        admin
            .disk
            .lock_unpoisoned()
            .sources
            .insert(source.to_owned(), market);
        admin
    }

    /// 「别的进程」改了磁盘：直接写用户层（rescan 才看得到）。
    pub(crate) fn write_elsewhere(&self, plugin: &str, enabled: bool) {
        self.disk
            .lock_unpoisoned()
            .user
            .entry(plugin.to_owned())
            .or_default()
            .enabled = Some(enabled);
    }

    fn market_of(disk: &Disk, plugin: &str) -> Option<(String, Market)> {
        let (name, market) = plugin.split_once('@')?;
        let source = disk.added.get(market)?;
        let market = disk.sources.get(source)?.clone();
        market
            .plugins
            .contains_key(name)
            .then(|| (name.to_owned(), market))
    }

    fn commit(&self, op: &PluginOp) -> Result<PluginApplied, PluginRefusal> {
        let mut disk = self.disk.lock_unpoisoned();
        let changed = match op {
            PluginOp::MarketplaceAdd(add) => {
                let Some(market) = disk.sources.get(&add.source).cloned() else {
                    return Err(PluginRefusal::Failed(format!("cannot read {}", add.source)));
                };
                disk.added.insert(market.name, add.source.clone()).is_none()
            }
            PluginOp::MarketplaceRemove(remove) => disk.added.remove(&remove.marketplace).is_some(),
            PluginOp::MarketplaceUpdate(_) => false,
            PluginOp::Install(install) => {
                if Self::market_of(&disk, &install.plugin_id).is_none() {
                    return Err(PluginRefusal::PluginNotFound(install.plugin_id.clone()));
                }
                if disk.installed.contains(&install.plugin_id) {
                    false
                } else {
                    disk.installed.push(install.plugin_id.clone());
                    // 同 Node：只补没写过的用户层启用
                    let layer = disk.user.entry(install.plugin_id.clone()).or_default();
                    layer.enabled.get_or_insert(true);
                    true
                }
            }
            PluginOp::Uninstall(uninstall) => {
                let before = disk.installed.len();
                disk.installed
                    .retain(|plugin| *plugin != uninstall.plugin_id);
                disk.installed.len() != before
            }
            PluginOp::Update(update) => {
                if !disk.installed.contains(&update.plugin_id) {
                    return Err(PluginRefusal::PluginNotFound(update.plugin_id.clone()));
                }
                false
            }
            PluginOp::SetEnabled { workspace, change } => {
                if !disk.installed.contains(&change.plugin_id) {
                    return Err(PluginRefusal::PluginNotFound(change.plugin_id.clone()));
                }
                let layer = layer(&mut disk, workspace.as_deref(), &change.plugin_id);
                let changed = layer.enabled != change.enabled;
                layer.enabled = change.enabled;
                changed
            }
            PluginOp::Configure { workspace, change } => {
                let Some((name, market)) = Self::market_of(&disk, &change.plugin_id) else {
                    return Err(PluginRefusal::PluginNotFound(change.plugin_id.clone()));
                };
                let declared = &market.plugins[&name];
                if let Some(key) = change
                    .set
                    .keys()
                    .chain(&change.clear)
                    .find(|key| !declared.contains_key(*key))
                {
                    return Err(PluginRefusal::OptionInvalid {
                        key: key.clone(),
                        message: format!("{key} is not declared by {name}"),
                    });
                }
                let layer = layer(&mut disk, workspace.as_deref(), &change.plugin_id);
                for (key, value) in &change.set {
                    if declared[key] {
                        let CommandPluginsConfigureSetValue::Alt1(text) = value else {
                            return Err(PluginRefusal::OptionInvalid {
                                key: key.clone(),
                                message: "a secret is a string".to_owned(),
                            });
                        };
                        layer.secrets.insert(key.clone(), text.clone());
                    } else {
                        layer.options.insert(key.clone(), option(value));
                    }
                }
                for key in &change.clear {
                    layer.options.remove(key);
                    layer.secrets.remove(key);
                }
                true
            }
        };
        Ok(PluginApplied {
            noop: !changed,
            diagnostics: Vec::new(),
        })
    }
}

fn option(value: &CommandPluginsConfigureSetValue) -> PluginOptionValue {
    match value {
        CommandPluginsConfigureSetValue::Alt1(text) => PluginOptionValue::Alt1(text.clone()),
        CommandPluginsConfigureSetValue::Alt2(number) => PluginOptionValue::Alt2(*number),
        CommandPluginsConfigureSetValue::Alt3(flag) => PluginOptionValue::Alt3(*flag),
    }
}

fn layer<'a>(disk: &'a mut Disk, workspace: Option<&str>, plugin: &str) -> &'a mut Layer {
    match workspace {
        Some(key) => disk
            .workspaces
            .entry(key.to_owned())
            .or_default()
            .entry(plugin.to_owned())
            .or_default(),
        None => disk.user.entry(plugin.to_owned()).or_default(),
    }
}

fn wire(layer: Option<&Layer>) -> PluginLayer {
    let layer = layer.cloned().unwrap_or_default();
    PluginLayer {
        enabled: layer.enabled,
        options: layer.options,
        secret_keys: layer.secrets.into_keys().collect(),
    }
}

async fn pass(gate: Option<Arc<Gate>>) {
    if let Some(gate) = gate {
        gate.entered.notify_one();
        gate.release.notified().await;
    }
}

#[async_trait]
impl PluginAdmin for FakeAdmin {
    async fn inventory(&self) -> PluginInventory {
        let inventory = {
            let disk = self.disk.lock_unpoisoned();
            let mut inventory = PluginInventory::default();
            for (name, source) in &disk.added {
                let market = &disk.sources[source];
                inventory.marketplaces.insert(
                    name.clone(),
                    PluginMarketplaceFields {
                        source: [("path".to_owned(), source.clone().into())].into(),
                        official: false,
                        description: None,
                        last_updated: None,
                        plugin_count: market.plugins.len() as u64,
                        featured: Vec::new(),
                        refresh_failure: None,
                    },
                );
                for (plugin, config) in &market.plugins {
                    let plugin_id = id(plugin, name);
                    inventory.catalog.insert(
                        plugin_id.clone(),
                        PluginCatalogEntryFields {
                            name: plugin.clone(),
                            marketplace: name.clone(),
                            description: None,
                            version: Some("1.0.0".to_owned()),
                            component_types: vec!["skill".to_owned()],
                            listing: None,
                            installed: disk.installed.contains(&plugin_id),
                            builtin: false,
                        },
                    );
                    if !disk.installed.contains(&plugin_id) {
                        continue;
                    }
                    let user = wire(disk.user.get(&plugin_id));
                    inventory.plugins.insert(
                        plugin_id.clone(),
                        PluginFields {
                            name: plugin.clone(),
                            marketplace: name.clone(),
                            origin: PluginFieldsOrigin::Marketplace,
                            version: Some("1.0.0".to_owned()),
                            description: None,
                            author: None,
                            author_url: None,
                            homepage: None,
                            root_path: format!("/Users/dev/plugins/cache/{name}/{plugin}"),
                            data_path: format!("/Users/dev/plugins/data/{plugin}-{name}"),
                            components: Vec::new(),
                            mcp_server_names: Vec::new(),
                            hooks: Vec::new(),
                            user_config: config
                                .iter()
                                .map(|(key, sensitive)| {
                                    (
                                        key.clone(),
                                        PluginFieldsUserConfigValue {
                                            default: None,
                                            description: None,
                                            required: None,
                                            sensitive: Some(*sensitive),
                                            title: None,
                                            r#type: None,
                                        },
                                    )
                                })
                                .collect(),
                            installed_at: None,
                            latest_version: None,
                            default_enabled: true,
                            enabled: user.enabled.unwrap_or(true),
                            user,
                        },
                    );
                }
            }
            inventory
        };
        let gate = self.read_gate.lock_unpoisoned().clone();
        pass(gate).await;
        inventory
    }

    async fn workspace(&self, key: &str) -> WorkspacePlugins {
        let disk = self.disk.lock_unpoisoned();
        let layers = disk.workspaces.get(key);
        WorkspacePlugins {
            plugins: disk
                .installed
                .iter()
                .map(|plugin| {
                    let workspace = wire(layers.and_then(|layers| layers.get(plugin)));
                    let user = disk.user.get(plugin).and_then(|layer| layer.enabled);
                    let enabled = workspace.enabled.or(user).unwrap_or(true);
                    (plugin.clone(), PluginWorkspaceFields { workspace, enabled })
                })
                .collect(),
            diagnostics: Vec::new(),
        }
    }

    async fn apply(
        &self,
        op: PluginOp,
        cancel: CancellationToken,
    ) -> Result<PluginApplied, PluginRefusal> {
        self.applied.lock_unpoisoned().push(op.clone());
        // 只有要下载的写挡在闸门（装、更新、加与刷新市场）：启停、配置、卸载、删市场只有锁内的读改写（24 §6.1）
        let fetches = matches!(
            op,
            PluginOp::Install(_)
                | PluginOp::Update(_)
                | PluginOp::MarketplaceAdd(_)
                | PluginOp::MarketplaceUpdate(_)
        );
        let gate = fetches
            .then(|| self.write_gate.lock_unpoisoned().clone())
            .flatten();
        // 提交点之前：「下载」挡在这里，取消在这里生效
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(PluginRefusal::Cancelled),
            () = pass(gate) => {}
        }
        self.commit(&op)
    }

    async fn describe(&self, plugin_id: &str) -> Result<QueryPluginsDescribeResult, PluginRefusal> {
        let disk = self.disk.lock_unpoisoned();
        let Some((name, _)) = Self::market_of(&disk, plugin_id) else {
            return Err(PluginRefusal::PluginNotFound(plugin_id.to_owned()));
        };
        Ok(QueryPluginsDescribeResult {
            components: vec![PluginFieldsComponentsItem {
                kind: PluginFieldsComponentsItemKind::Skill,
                items: vec![PluginFieldsComponentsItemItemsItem {
                    name,
                    description: None,
                }],
            }],
            metadata: None,
            diagnostics: Vec::new(),
        })
    }
}
