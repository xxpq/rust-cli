//! 服务的写操作（plugin-store.md §5、§8、§9、§10）。形状都是：锁外取内容（`Fetching`，可取消）→ 拿存储锁
//! （`WaitingForLock`，可取消）→ 持锁提交（`Committing`，不可取消）：先存储，再 `config.json`（platform 的持锁写者），再私有
//! 文件（锁序，§4）。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rust_cli_plugin_store::core::catalog::Listing;
use rust_cli_plugin_store::core::config_patch;
use rust_cli_plugin_store::core::declared;
use rust_cli_plugin_store::core::diagnostic::{Diagnostic, StoreError};
use rust_cli_plugin_store::core::id::{OFFICIAL_MARKETPLACE, PluginId, is_official_marketplace};
use rust_cli_plugin_store::core::records::{Known, Record};
use rust_cli_plugin_store::core::secrets::{self, project_key};
use rust_cli_plugin_store::core::source::{MarketplaceSource, SourceInput, classify_input};
use rust_cli_plugin_store::local::Locked;
use serde_json::{Map, Value};

use super::{Context, Control, Phase, PluginAdmin, Scope};

/// 一个装好（或卸掉）的插件的视图（Node `ZCodeInstalledPluginData`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledView {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) marketplace: String,
    pub(crate) scope: String,
    pub(crate) version: String,
    pub(crate) install_path: String,
    pub(crate) installed_at: Option<String>,
    pub(crate) enabled: bool,
}

/// 安装与更新的结果：失败也在这里（error 级诊断，Node 相同），另留有类型的错误（v5 的端口按它给拒绝码）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InstallOutcome {
    pub(crate) closure: Vec<String>,
    pub(crate) installed: Vec<InstalledView>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// 失败的原因（有它时 `diagnostics` 里是它的那一条）。
    pub(crate) error: Option<StoreError>,
    /// 什么也没变：闭包里每个插件都装回了原来的目录（同一份内容），配置也没改（设值语义的 noop）。
    pub(crate) unchanged: bool,
}

impl InstallOutcome {
    fn failed(error: StoreError, plugin_id: &str) -> Self {
        Self {
            diagnostics: vec![error.to_diagnostic(Some(plugin_id))],
            error: Some(error),
            ..Self::default()
        }
    }
}

/// 卸掉的插件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Removed {
    pub(crate) view: InstalledView,
}

/// 启停的结果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SetEnabled {
    pub(crate) enabled: Option<bool>,
    pub(crate) path: String,
    pub(crate) plugin: super::Listed,
    /// 配置文件改了（写同样的值时没改）。
    pub(crate) changed: bool,
}

/// 可装的条目（Node `ZCodeAvailablePluginData`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Available {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) marketplace: String,
    pub(crate) version: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) installed: bool,
    /// 商店的展示字段与条目声明的组件种类（插件管理页用，§12.1；子命令不输出）。
    pub(crate) listing: Option<Listing>,
    pub(crate) component_types: Vec<String>,
}

/// 市场与可装的条目（`list --available`、`marketplace list`、裸名字的安装）。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CatalogView {
    pub(crate) marketplaces: Vec<Known>,
    pub(crate) available: Vec<Available>,
    pub(crate) restorable: Vec<Available>,
    /// 市场 id → 清单里的策展名单（读得到清单缓存的才有）。
    pub(crate) featured: std::collections::BTreeMap<String, Vec<String>>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

/// 市场刷新的结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Refreshed {
    pub(crate) marketplaces: Vec<Known>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

fn view(record: &Record, enabled: bool) -> InstalledView {
    InstalledView {
        id: record.id.clone(),
        name: record.name.clone(),
        marketplace: record.marketplace.clone(),
        scope: record.scope.clone(),
        version: record.version.clone(),
        install_path: record.install_path.clone(),
        installed_at: Some(record.installed_at.clone()),
        enabled,
    }
}

fn config_error(error: rust_cli_platform::local::ConfigWriteError) -> StoreError {
    StoreError::Invalid(error.to_string())
}

impl PluginAdmin {
    /// 拿存储锁（阶段 `WaitingForLock`）。
    async fn lock<'a>(
        &self,
        context: &'a Context,
        control: &Control,
    ) -> Result<Locked<'a>, StoreError> {
        control.enter(Phase::WaitingForLock);
        let locked = context.store.lock(&control.cancel).await?;
        control.enter(Phase::Committing);
        Ok(locked)
    }

    /// 持锁时改一份配置文件（锁序的第二把，§4）。
    async fn edit_config(
        &self,
        file: &Path,
        edit: impl FnOnce(&mut Map<String, Value>) -> bool,
    ) -> Result<(), StoreError> {
        rust_cli_platform::local::update_config_object(file, edit)
            .await
            .map_err(config_error)
    }

    /// 市场与可装的条目（§12 `catalog`）。缺省市场没登记时先登记（Node 的 overview 相同，会拿一次锁）。用户级声明的市场合进来
    /// （§9.5）：只有声明、或声明改指了来源的，没有清单可读。
    pub(crate) async fn catalog(&self, cwd: &str, control: &Control) -> CatalogView {
        self.catalog_in(&self.context(cwd).await, control).await
    }

    pub(super) async fn catalog_in(&self, context: &Context, control: &Control) -> CatalogView {
        let mut view = CatalogView::default();
        if let Err(error) = self.ensure_defaults(context, control).await {
            view.diagnostics.push(error.to_diagnostic(None));
        }
        let known = context
            .store
            .known_marketplaces()
            .await
            .unwrap_or_else(|error| {
                view.diagnostics.push(error.to_diagnostic(None));
                Vec::new()
            });
        let (listed, reserved) = declared::merge(known, Self::declared(context));
        view.diagnostics.extend(reserved);
        let installed: BTreeSet<String> = context
            .store
            .installed_records()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.id)
            .collect();
        for declared::Listed { record, cached } in &listed {
            if let Some(failure) = &record.refresh_failure {
                let message = failure
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                view.diagnostics
                    .push(StoreError::Fetch(message.to_owned()).to_diagnostic(Some(&record.id)));
            }
            if !cached {
                continue;
            }
            let Ok(Some(loaded)) = context.store.catalog(&record.id).await else {
                continue;
            };
            if !loaded.catalog.featured.is_empty() {
                view.featured
                    .insert(record.id.clone(), loaded.catalog.featured.clone());
            }
            for entry in &loaded.catalog.plugins {
                let id = PluginId::new(&entry.name, &record.id).to_string();
                view.available.push(Available {
                    installed: installed.contains(&id),
                    id,
                    name: entry.name.clone(),
                    marketplace: record.id.clone(),
                    version: entry.version.clone(),
                    description: entry.description.clone(),
                    listing: entry.listing(),
                    component_types: entry.component_types(),
                });
            }
        }
        let suppressed = &context.config.plugins.section.suppressed_builtins;
        for (id, builtin) in Self::builtins(context).await {
            if !suppressed.contains(&id) {
                continue;
            }
            let manifest = &builtin.manifest;
            let text = |key: &str| manifest.get(key).and_then(Value::as_str).map(str::to_owned);
            let name = id
                .rsplit_once('@')
                .map_or(id.as_str(), |(name, _)| name)
                .to_owned();
            view.restorable.push(Available {
                id: id.clone(),
                name,
                marketplace: OFFICIAL_MARKETPLACE.to_owned(),
                version: text("version"),
                description: text("description"),
                installed: false,
                listing: None,
                component_types: Vec::new(),
            });
        }
        view.marketplaces = listed.into_iter().map(|item| item.record).collect();
        view
    }

    /// 用户级声明的市场（§9.5）。
    fn declared(context: &Context) -> &std::collections::BTreeMap<String, MarketplaceSource> {
        &context.config.plugins.section.declared_marketplaces
    }

    /// 按声明加一个市场（显式刷新、安装之前，§9.5）：同 `marketplace add`，清单的名字要等于声明的 id。
    async fn materialize(
        &self,
        context: &Context,
        id: &str,
        source: &MarketplaceSource,
        control: &Control,
    ) -> Result<Known, StoreError> {
        control.enter(Phase::Fetching);
        let staged = context
            .store
            .prepare_marketplace(source.clone(), Some(id), &control.cancel)
            .await?;
        let locked = match self.lock(context, control).await {
            Ok(locked) => locked,
            Err(error) => {
                staged.discard().await;
                return Err(error);
            }
        };
        let result = async {
            locked.ensure_default_marketplaces(self.now_ms()).await?;
            locked.commit_marketplace(staged, self.now_ms()).await
        }
        .await;
        locked.release().await;
        result
    }

    async fn ensure_defaults(
        &self,
        context: &Context,
        control: &Control,
    ) -> Result<(), StoreError> {
        let known = context.store.known_marketplaces().await?;
        if rust_cli_plugin_store::core::records::DEFAULT_MARKETPLACES
            .iter()
            .all(|(id, ..)| known.iter().any(|record| record.id == *id))
        {
            return Ok(());
        }
        let locked = self.lock(context, control).await?;
        let result = locked.ensure_default_marketplaces(self.now_ms()).await;
        locked.release().await;
        result
    }

    /// 刷新一个已知市场：锁外取清单；成功锁内提交，失败锁内记下失败（Node `updateMarketplace`）。
    async fn refresh_one(
        &self,
        context: &Context,
        record: &Known,
        control: &Control,
    ) -> Result<Known, StoreError> {
        control.enter(Phase::Fetching);
        let staged = context
            .store
            .prepare_marketplace(record.source.clone(), Some(&record.id), &control.cancel)
            .await;
        let locked = match self.lock(context, control).await {
            Ok(locked) => locked,
            Err(error) => {
                if let Ok(staged) = staged {
                    staged.discard().await;
                }
                return Err(error);
            }
        };
        let result = match staged {
            Ok(staged) => locked.commit_marketplace(staged, self.now_ms()).await,
            Err(error) => locked
                .record_refresh_failure(&record.id, &error, self.now_ms())
                .await
                .and(Err(error)),
        };
        locked.release().await;
        result
    }

    /// 刷新市场（§9.2）：给了名字只刷它（不认识时报错），否则刷全部已知的。一个失败不挡别的。给了名字、是用户级声明的市场
    /// （§9.5）：还没有已知记录时按声明加；已知记录的来源与声明不同时不刷新，报一条诊断。全部刷新不物化声明。
    pub(crate) async fn marketplace_refresh(
        &self,
        cwd: &str,
        name: Option<&str>,
        control: &Control,
    ) -> Result<Refreshed, StoreError> {
        let context = self.context(cwd).await;
        self.ensure_defaults(&context, control).await?;
        let known = context.store.known_marketplaces().await?;
        if let Some(name) = name {
            match declared::to_materialize(&known, Self::declared(&context), name) {
                Ok(Some(source)) => {
                    let mut refreshed = Refreshed::default();
                    match self.materialize(&context, name, source, control).await {
                        Ok(record) => refreshed.marketplaces.push(record),
                        Err(StoreError::Cancelled) => return Err(StoreError::Cancelled),
                        Err(error) => refreshed.diagnostics.push(error.to_diagnostic(Some(name))),
                    }
                    return Ok(refreshed);
                }
                Ok(None) => {}
                Err(error) => {
                    return Ok(Refreshed {
                        diagnostics: vec![error.to_diagnostic(Some(name))],
                        ..Refreshed::default()
                    });
                }
            }
        }
        let targets: Vec<&Known> = known
            .iter()
            .filter(|record| name.is_none_or(|name| record.id == name))
            .collect();
        if let Some(name) = name
            && targets.is_empty()
        {
            return Err(StoreError::MarketplaceNotFound(name.to_owned()));
        }
        let mut refreshed = Refreshed::default();
        for record in targets {
            match self.refresh_one(&context, record, control).await {
                Ok(record) => refreshed.marketplaces.push(record),
                Err(StoreError::Cancelled) => return Err(StoreError::Cancelled),
                Err(error) => refreshed
                    .diagnostics
                    .push(error.to_diagnostic(Some(&record.id))),
            }
        }
        Ok(refreshed)
    }

    /// 加一个市场（§9.1、§9.2）。
    pub(crate) async fn marketplace_add(
        &self,
        cwd: &str,
        input: &str,
        sparse: &[String],
        control: &Control,
    ) -> Result<Known, StoreError> {
        let context = self.context(cwd).await;
        let source = match classify_input(input)? {
            SourceInput::Remote(source) => source,
            SourceInput::Path(path) => self.local_source(cwd, &path).await?,
        }
        .with_sparse(sparse)?;
        control.enter(Phase::Fetching);
        let staged = context
            .store
            .prepare_marketplace(source, None, &control.cancel)
            .await?;
        let locked = match self.lock(&context, control).await {
            Ok(locked) => locked,
            Err(error) => {
                staged.discard().await;
                return Err(error);
            }
        };
        let result = async {
            locked.ensure_default_marketplaces(self.now_ms()).await?;
            locked.commit_marketplace(staged, self.now_ms()).await
        }
        .await;
        locked.release().await;
        result
    }

    /// 本地路径的来源（Node `resolvePathInput`）：`~` 按 home，其余相对 cwd；目录或 `.json` 文件。
    async fn local_source(&self, cwd: &str, input: &str) -> Result<MarketplaceSource, StoreError> {
        let path = match input.strip_prefix('~') {
            Some(rest) => self.home.join(rest.trim_start_matches(['/', '\\'])),
            None => Path::new(cwd).join(input),
        };
        let path = rust_cli_base::path::normalize(&path);
        let shown = path.display().to_string();
        if rust_cli_fs::is_dir(&path).await {
            return Ok(MarketplaceSource::local(shown, true));
        }
        if rust_cli_fs::is_file(&path).await {
            if !shown.ends_with(".json") {
                return Err(StoreError::Invalid(format!(
                    "Marketplace file must be a .json file: {shown}"
                )));
            }
            return Ok(MarketplaceSource::local(shown, false));
        }
        Err(StoreError::Invalid(format!(
            "Marketplace source path does not exist: {shown}"
        )))
    }

    /// 删一个市场（Node 相同：不删清单缓存与已装的插件）。
    pub(crate) async fn marketplace_remove(
        &self,
        cwd: &str,
        name: &str,
        control: &Control,
    ) -> Result<bool, StoreError> {
        let context = self.context(cwd).await;
        let locked = self.lock(&context, control).await?;
        let removed = locked.remove_marketplace(name).await;
        locked.release().await;
        removed
    }

    /// 安装（§5.1、§5.4）。`id` 已带市场（裸名字由命令层按可装的条目解析）。
    pub(crate) async fn install(
        &self,
        cwd: &str,
        id: &PluginId,
        control: &Control,
    ) -> InstallOutcome {
        let context = self.context(cwd).await;
        let label = id.to_string();
        let suppressed = context
            .config
            .plugins
            .section
            .suppressed_builtins
            .contains(&label);
        if id.marketplace == OFFICIAL_MARKETPLACE
            && suppressed
            && Self::builtins(&context).await.contains_key(&label)
        {
            return match self.restore(&context, &label, control).await {
                Ok(()) => {
                    let listing = self.listing(&self.context(cwd).await).await;
                    let installed = listing
                        .plugins
                        .iter()
                        .find(|p| p.id == label)
                        .map(|plugin| InstalledView {
                            id: plugin.id.clone(),
                            name: plugin.name.clone(),
                            marketplace: plugin.marketplace.clone(),
                            scope: "user".to_owned(),
                            version: plugin.version.clone().unwrap_or_default(),
                            install_path: plugin.root.clone(),
                            installed_at: None,
                            enabled: plugin.enabled,
                        });
                    InstallOutcome {
                        closure: vec![label],
                        installed: installed.into_iter().collect(),
                        ..InstallOutcome::default()
                    }
                }
                Err(error) => InstallOutcome::failed(error, &label),
            };
        }
        match self.install_from_catalog(&context, id, control).await {
            Ok(outcome) => outcome,
            Err(error) => InstallOutcome::failed(error, &label),
        }
    }

    async fn install_from_catalog(
        &self,
        context: &Context,
        id: &PluginId,
        control: &Control,
    ) -> Result<InstallOutcome, StoreError> {
        self.ensure_defaults(context, control).await?;
        // 用户级声明的市场（§9.5）：已知记录的来源不同时不装；还没有已知记录时先按声明加
        let known = context.store.known_marketplaces().await?;
        if let Some(source) =
            declared::to_materialize(&known, Self::declared(context), &id.marketplace)?
        {
            self.materialize(context, &id.marketplace, source, control)
                .await?;
        }
        // 清单缓存缺了时先刷新这个市场（Node `ensureMarketplaceManifestAvailable`）。
        if context.store.catalog(&id.marketplace).await?.is_none() {
            let known = context.store.known_marketplaces().await?;
            let record = known
                .iter()
                .find(|record| record.id == id.marketplace)
                .ok_or_else(|| StoreError::MarketplaceNotFound(id.marketplace.clone()))?;
            self.refresh_one(context, record, control).await?;
        }
        control.enter(Phase::Fetching);
        let prepared = context.store.prepare_install(id, &control.cancel).await?;
        let closure: Vec<String> = prepared.closure.iter().map(ToString::to_string).collect();
        let locked = match self.lock(context, control).await {
            Ok(locked) => locked,
            Err(error) => {
                prepared.discard().await;
                return Err(error);
            }
        };
        let result = self.commit_install(context, &locked, prepared).await;
        locked.release().await;
        let (installed, unchanged) = result?;
        Ok(InstallOutcome {
            closure,
            installed,
            unchanged,
            ..InstallOutcome::default()
        })
    }

    /// 提交安装：返回装好的插件与「什么也没变」（每个都装回原来的目录、配置没改）。
    async fn commit_install(
        &self,
        context: &Context,
        locked: &Locked<'_>,
        prepared: rust_cli_plugin_store::local::install::Prepared,
    ) -> Result<(Vec<InstalledView>, bool), StoreError> {
        let committed = locked.commit_install(prepared, self.now_ms()).await?;
        let ids: Vec<String> = committed.iter().map(|item| item.id.to_string()).collect();
        let same_dirs = committed.iter().all(|item| {
            item.previous
                .as_ref()
                .is_some_and(|previous| Path::new(&previous.install_path) == item.install_path)
        });
        let user = self.config_file(Scope::User, &context.cwd);
        let mut fresh = Vec::new();
        let mut config_changed = false;
        self.edit_config(&user, |config| {
            fresh = config_patch::default_enable(config, &ids);
            let mut changed = !fresh.is_empty();
            // 官方市场与内置插件共用 id：显式装了，就去掉以前的隐藏（Node 相同）。
            for id in ids
                .iter()
                .filter(|id| id.ends_with(&format!("@{OFFICIAL_MARKETPLACE}")))
            {
                changed |= config_patch::unsuppress(config, id);
            }
            config_changed = changed;
            changed
        })
        .await?;
        let records = locked_records(locked).await?;
        let enabled_plugins = &context.config.plugins.section.enabled_plugins;
        Ok(committed
            .iter()
            .filter_map(|item| {
                let id = item.id.to_string();
                let record = records.iter().find(|record| record.id == id)?;
                let enabled =
                    fresh.contains(&id) || enabled_plugins.get(&id).copied().unwrap_or(false);
                Some(view(record, enabled))
            })
            .collect::<Vec<_>>())
        .map(|installed| (installed, same_dirs && !config_changed))
    }

    /// 更新（§5.2）：要已装；先刷新所属的市场，失败就停；再按同一个条目装。返回旧版本。
    pub(crate) async fn update(
        &self,
        cwd: &str,
        id: &str,
        control: &Control,
    ) -> Result<(InstallOutcome, String), StoreError> {
        let context = self.context(cwd).await;
        let record = context
            .store
            .installed_records()
            .await?
            .into_iter()
            .find(|record| record.id == id)
            .ok_or_else(|| StoreError::Invalid(format!("Plugin not installed: {id}")))?;
        let known = context.store.known_marketplaces().await?;
        let Some(marketplace) = known.iter().find(|known| known.id == record.marketplace) else {
            let error = StoreError::MarketplaceNotFound(record.marketplace.clone());
            return Ok((InstallOutcome::failed(error, id), record.version));
        };
        if let Err(error) = self.refresh_one(&context, marketplace, control).await {
            return Ok((
                InstallOutcome::failed(error, &record.marketplace),
                record.version,
            ));
        }
        let plugin = PluginId::new(&record.name, &record.marketplace);
        let outcome = match self
            .install_from_catalog(&self.context(cwd).await, &plugin, control)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => InstallOutcome::failed(error, id),
        };
        Ok((outcome, record.version))
    }

    /// 卸载（§5.3）。没有这个插件（不在记录里、也不是内置）时为 None。
    pub(crate) async fn uninstall(
        &self,
        cwd: &str,
        id: &str,
        keep_data: bool,
        control: &Control,
    ) -> Result<Option<Removed>, StoreError> {
        let context = self.context(cwd).await;
        let builtin = Self::builtins(&context)
            .await
            .get(id)
            .map(|builtin| builtin.root.clone());
        let locked = self.lock(&context, control).await?;
        let result = self
            .uninstall_locked(&context, &locked, id, keep_data, builtin)
            .await;
        locked.release().await;
        result
    }

    async fn uninstall_locked(
        &self,
        context: &Context,
        locked: &Locked<'_>,
        id: &str,
        keep_data: bool,
        builtin: Option<PathBuf>,
    ) -> Result<Option<Removed>, StoreError> {
        let user = self.config_file(Scope::User, &context.cwd);
        let removed = match locked.remove_install(id, self.now_ms()).await? {
            Some(record) => {
                self.edit_config(&user, |config| {
                    let removed = config_patch::remove_plugin(config, id);
                    config_patch::unsuppress(config, id) | removed
                })
                .await?;
                view(&record, false)
            }
            None => {
                let Some(root) = builtin else {
                    return Ok(None);
                };
                self.edit_config(&user, |config| {
                    let removed = config_patch::remove_plugin(config, id);
                    config_patch::suppress(config, id) | removed
                })
                .await?;
                let (name, marketplace) = id.rsplit_once('@').unwrap_or((id, ""));
                InstalledView {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    marketplace: marketplace.to_owned(),
                    scope: "user".to_owned(),
                    version: String::new(),
                    install_path: root.display().to_string(),
                    installed_at: None,
                    enabled: false,
                }
            }
        };
        locked.update_secrets(|file| file.remove(id, None)).await?;
        if !keep_data {
            locked.remove_data(id).await?;
        }
        Ok(Some(Removed { view: removed }))
    }

    /// 恢复内置（§5.4）：从用户级的 `suppressedBuiltins` 去掉。子命令与 v5 都经 `install` 走到这里（24 §3.4）。
    async fn restore(
        &self,
        context: &Context,
        id: &str,
        control: &Control,
    ) -> Result<(), StoreError> {
        let locked = self.lock(context, control).await?;
        let user = self.config_file(Scope::User, &context.cwd);
        let result = self
            .edit_config(&user, |config| config_patch::unsuppress(config, id))
            .await;
        locked.release().await;
        result
    }

    /// 启停（§10）：`None` 删键（恢复继承）。插件按列出的条目找（id 或唯一的名字）。
    pub(crate) async fn set_enabled(
        &self,
        cwd: &str,
        selector: &str,
        scope: Scope,
        enabled: Option<bool>,
        control: &Control,
    ) -> Result<SetEnabled, StoreError> {
        let context = self.context(cwd).await;
        let listing = self.listing(&context).await;
        let mut plugin = listing.select(selector)?.clone();
        let file = self.config_file(scope, cwd);
        let locked = self.lock(&context, control).await?;
        let mut changed = false;
        let result = self
            .edit_config(&file, |config| match enabled {
                // Node 写同样的值也重写（键移到末尾）；「改没改」按写之前的值算
                Some(value) => {
                    changed = config_patch::enabled_of(config, &plugin.id) != Some(value);
                    config_patch::set_enabled(config, &plugin.id, value)
                }
                None => {
                    changed = config_patch::clear_enabled(config, &plugin.id);
                    changed
                }
            })
            .await;
        locked.release().await;
        result?;
        if let Some(value) = enabled {
            plugin.enabled = value;
        }
        Ok(SetEnabled {
            enabled,
            path: file.display().to_string(),
            plugin,
            changed,
        })
    }

    /// 配置值（§10，Node `savePluginOptions`）：敏感键（manifest `userConfig[k].sensitive`）进私有文件，普通键进
    /// `config.json`；这一层 `config.json` 里已有的敏感明文值一起迁走。先私有文件后 `config.json`。返回有没有改文件（写同样的值
    /// 时没改）。
    pub(crate) async fn configure(
        &self,
        cwd: &str,
        selector: &str,
        scope: Scope,
        (set, clear): (&Map<String, Value>, &[String]),
        control: &Control,
    ) -> Result<bool, StoreError> {
        let context = self.context(cwd).await;
        let listing = self.listing(&context).await;
        let plugin = listing.select(selector)?.clone();
        let sensitive: BTreeSet<String> = plugin
            .user_config
            .as_ref()
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(_, option)| {
                option.get(rust_cli_base::plugin::SENSITIVE_KEY) == Some(&Value::Bool(true))
            })
            .map(|(key, _)| key.clone())
            .collect();
        let scalar = |value: &&Value| value.is_string() || value.is_number() || value.is_boolean();
        let file = self.config_file(scope, cwd);
        let locked = self.lock(&context, control).await?;
        let result = async {
            let current = rust_cli_fs::read_optional(&file)
                .await
                .ok()
                .flatten()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            let mut secret_values = Map::new();
            for (key, value) in config_patch::options_of(&current, &plugin.id) {
                if sensitive.contains(&key) && !clear.contains(&key) {
                    secret_values.insert(key, value);
                }
            }
            let mut plain = Map::new();
            for (key, value) in set.iter().filter(|(_, value)| scalar(value)) {
                if sensitive.contains(key) {
                    secret_values.insert(key.clone(), value.clone());
                } else {
                    plain.insert(key.clone(), value.clone());
                }
            }
            let mut secret_clear: Vec<String> = clear.to_vec();
            secret_clear.extend(plain.keys().cloned());
            let layer = match scope {
                Scope::User => secrets::Scope::User,
                Scope::Workspace => secrets::Scope::Workspace(project_key(cwd)),
            };
            let mut changed = false;
            if !secret_values.is_empty() || !secret_clear.is_empty() {
                locked
                    .update_secrets(|secrets| {
                        let before = secrets.to_text();
                        secrets.set(&layer, &plugin.id, &secret_values, &secret_clear);
                        changed = secrets.to_text() != before;
                    })
                    .await?;
            }
            let mut plain_clear: Vec<String> = clear.to_vec();
            plain_clear.extend(secret_values.keys().cloned());
            self.edit_config(&file, |config| {
                let patched = config_patch::patch_options(config, &plugin.id, &plain, &plain_clear);
                changed |= patched;
                patched
            })
            .await?;
            Ok(changed)
        }
        .await;
        locked.release().await;
        result
    }

    /// 孤儿目录的清理（§6.3）：`in_use` 是这个进程里还在用的插件根。
    pub(crate) async fn sweep_orphans(
        &self,
        cwd: &str,
        in_use: &BTreeSet<PathBuf>,
        control: &Control,
    ) -> Result<Vec<PathBuf>, StoreError> {
        let context = self.context(cwd).await;
        let locked = self.lock(&context, control).await?;
        let result = locked.sweep_orphans(self.now_ms(), in_use).await;
        locked.release().await;
        result
    }

    /// 已知的官方市场 id（`marketplace list` 的 `isOfficial`）。
    pub(crate) fn is_official(id: &str) -> bool {
        is_official_marketplace(id)
    }
}

/// 持锁时的记录（提交之后读回，拿 `installedAt`）。
async fn locked_records(locked: &Locked<'_>) -> Result<Vec<Record>, StoreError> {
    locked.records().await
}
