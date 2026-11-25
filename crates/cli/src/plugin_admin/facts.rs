//! 服务的读（plugin-store.md §12.1）：插件管理页要的事实——全部已装、内置与 `plugins.dirs` 的插件包，每一层的启停与配置值，
//! 市场与目录。不知道 v5：结果是服务自己的类型，前端各自换形状（v5 在 `frontends/app_plugins.rs`）。每次读整份，不缓存。
//!
//! 包的事实与启停无关：按「全部启用、不建数据目录」加载一次，停用的插件也读出组件。生效值按加载的同一条规则算
//! （`rust_cli_ext::plugins::default_enabled`）：用户级是「用户层 ?? 缺省」，工作区级是这个工作区合并后的配置。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rust_cli_ext::plugins::{
    Plugin, PluginOrigin, PluginSet, PluginSources, SkipReason, default_enabled, load,
};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_plugin_store::core::catalog::author;
use rust_cli_plugin_store::core::config_patch;
use rust_cli_plugin_store::core::diagnostic::{Diagnostic, StoreError, code};
use rust_cli_plugin_store::core::id::PluginId;
use rust_cli_plugin_store::core::manifest::{self, MANIFEST_PATHS};
use rust_cli_plugin_store::core::secrets::{SecretValues, project_key};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use super::listing::{HookDetail, SKILL_FILE, declared_mcp, hook_details};
use super::writes::CatalogView;
use super::{Context, Control, PluginAdmin, Scope};
use crate::assemble::env_layout::EnvLayout;
use crate::config::plugins::sources;

/// 插件包从哪来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    /// 从市场装的（`installed_plugins.json`）。
    Marketplace,
    /// App 随包的只读目录，或 Node 播种的内置缓存。
    Builtin,
    /// 配置 `plugins.dirs` 的目录。
    Directory,
}

/// 一层配置里这个插件的事实。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Layer {
    /// 这一层写了的启停；没有 = 继承下一层。
    pub(crate) enabled: Option<bool>,
    /// 普通值。manifest 标了敏感的键不在这里，即使旧数据把它明文写在 `config.json` 里（值不出服务）。
    pub(crate) options: Map<String, Value>,
    /// 这一层有值的敏感键：私有文件里的，与明文写在 `config.json` 里、manifest 标了敏感的。
    pub(crate) secret_keys: Vec<String>,
}

/// 组件的种类（详情页的分组顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComponentKind {
    Agent,
    Command,
    Skill,
    Hook,
    Mcp,
}

/// 一个插件包：包的事实、用户层与用户级生效值。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Package {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) marketplace: String,
    pub(crate) origin: Origin,
    pub(crate) manifest: Value,
    pub(crate) root: String,
    /// 数据目录（`${CLAUDE_PLUGIN_DATA}` 同值）；没有数据根时为空。
    pub(crate) data_path: String,
    /// 组件的名字（不带描述，24 §12 第 13 项），只有非空的组。
    pub(crate) components: Vec<(ComponentKind, Vec<String>)>,
    /// 运行时的 MCP 名（`plugin:<插件>:<server>`）。
    pub(crate) mcp_servers: Vec<String>,
    pub(crate) hooks: Vec<HookDetail>,
    pub(crate) installed_at: Option<String>,
    /// 市场条目的版本与装的不同时（24 §12 第 15 项）。
    pub(crate) latest_version: Option<String>,
    pub(crate) default_enabled: bool,
    pub(crate) user: Layer,
    /// 用户级生效值：`user.enabled ?? default_enabled`。
    pub(crate) enabled: bool,
}

impl Package {
    /// manifest 的字符串字段。
    pub(crate) fn text(&self, key: &str) -> Option<String> {
        self.manifest
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// manifest 的作者与作者主页。
    pub(crate) fn author(&self) -> (Option<String>, Option<String>) {
        author(self.manifest.get("author"))
    }
}

/// 用户级的读。
#[derive(Debug, Clone, Default)]
pub(crate) struct Inventory {
    pub(crate) packages: Vec<Package>,
    pub(crate) catalog: CatalogView,
    /// 读不出的市场、坏的插件包（加载的错误表）。
    pub(crate) diagnostics: Vec<Diagnostic>,
}

/// 一个工作区的读：每个插件在这个工作区的一层与生效值，跳过加载的原因。
#[derive(Debug, Clone, Default)]
pub(crate) struct WorkspaceFacts {
    pub(crate) plugins: Vec<(String, Layer, bool)>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

/// `plugins.describe` 的读：组件与元数据。
#[derive(Debug, Clone, Default)]
pub(crate) struct Described {
    pub(crate) components: Vec<(ComponentKind, Vec<String>)>,
    pub(crate) manifest: Value,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

/// 没有加载、但仍是一个插件包的跳过原因：依赖 Node 运行时、前端没声明它要的能力、与已加载的插件同名。别的（停用之外的坏目录、
/// 隐藏的内置、被装的盖过的内置、重复的 id）不是独立的包。
fn still_a_package(reason: &SkipReason) -> bool {
    matches!(
        reason,
        SkipReason::NodeRuntime(_) | SkipReason::FrontendLacks(_) | SkipReason::DuplicateName
    )
}

fn origin(origin: PluginOrigin) -> Origin {
    match origin {
        PluginOrigin::Installed => Origin::Marketplace,
        PluginOrigin::Official => Origin::Builtin,
        PluginOrigin::Session | PluginOrigin::ConfigDir => Origin::Directory,
    }
}

/// manifest 里标了敏感的配置键。
fn sensitive(manifest: &Value) -> BTreeSet<String> {
    manifest
        .get("userConfig")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, option)| {
            option.get(rust_cli_base::plugin::SENSITIVE_KEY) == Some(&Value::Bool(true))
        })
        .map(|(key, _)| key.clone())
        .collect()
}

/// 一层的事实：`config` 是这一层配置文件的顶层对象，`secrets` 是私有文件里这一层对应的部分。
fn layer(
    config: &Map<String, Value>,
    secrets: &[&SecretValues],
    id: &str,
    manifest: &Value,
) -> Layer {
    let sensitive = sensitive(manifest);
    let mut found = Layer {
        enabled: config_patch::enabled_of(config, id),
        ..Layer::default()
    };
    let mut keys = BTreeSet::new();
    for (key, value) in config_patch::options_of(config, id) {
        if sensitive.contains(&key) {
            keys.insert(key);
        } else {
            found.options.insert(key, value);
        }
    }
    for values in secrets {
        keys.extend(values.get(id).into_iter().flat_map(BTreeMap::keys).cloned());
    }
    found.secret_keys = keys.into_iter().collect();
    found
}

/// 一个配置文件的顶层对象；没有或读不出时是空的（读不出的文件由配置的组装记警告）。
async fn config_object(file: &Path) -> Map<String, Value> {
    rust_cli_fs::read_optional(file)
        .await
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// 一个目录的 manifest（找不到或读不出时为 Null）。
async fn read_manifest(dir: &str) -> Value {
    for path in MANIFEST_PATHS {
        let file = Path::new(dir).join(path);
        if let Ok(Some(bytes)) = rust_cli_fs::read_optional(&file).await {
            return serde_json::from_slice(&bytes).unwrap_or_default();
        }
    }
    Value::Null
}

/// 目录里的 `.md` 文件名（去掉后缀）。
async fn markdown_names(dir: &Path) -> Vec<String> {
    rust_cli_fs::file_names(dir)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|name| name.strip_suffix(MARKDOWN).map(str::to_owned))
        .collect()
}

const MARKDOWN: &str = ".md";

/// 一个路径的最后一段（不带 `.md`）。
fn stem(path: &str) -> String {
    let name = PathStyle::infer(path).file_name(path);
    name.strip_suffix(MARKDOWN)
        .map_or(name.clone(), str::to_owned)
}

/// 组件的名字（skill 的目录名、命令与 agent 的文件名、hook 的事件、MCP 服务器名），按详情页的分组顺序，只留非空的组。
async fn components(plugin: &Plugin, hooks: &[HookDetail]) -> Vec<(ComponentKind, Vec<String>)> {
    let agents = plugin
        .agent_files
        .iter()
        .map(|file| stem(&file.path))
        .collect();
    let mut commands = Vec::new();
    for root in &plugin.command_roots {
        match root {
            rust_cli_ext::plugins::PluginCommandRoot::Dir(dir) => {
                commands.extend(markdown_names(Path::new(dir)).await);
            }
            rust_cli_ext::plugins::PluginCommandRoot::File { path, name, .. } => {
                commands.push(name.clone().unwrap_or_else(|| stem(path)));
            }
        }
    }
    let mut skills = Vec::new();
    for dir in &plugin.skill_dirs {
        let path = Path::new(dir);
        if rust_cli_fs::is_file(&path.join(SKILL_FILE)).await {
            skills.push(PathStyle::infer(dir).file_name(dir));
            continue;
        }
        for child in rust_cli_fs::subdirectories(path).await.unwrap_or_default() {
            if rust_cli_fs::is_file(&path.join(&child).join(SKILL_FILE)).await {
                skills.push(child);
            }
        }
    }
    let mut events: Vec<String> = Vec::new();
    for hook in hooks {
        if !events.contains(&hook.event) {
            events.push(hook.event.clone());
        }
    }
    [
        (ComponentKind::Agent, agents),
        (ComponentKind::Command, commands),
        (ComponentKind::Skill, skills),
        (ComponentKind::Hook, events),
        (ComponentKind::Mcp, declared_mcp(plugin)),
    ]
    .into_iter()
    .filter(|(_, names)| !names.is_empty())
    .collect()
}

/// 候选目录的 id（manifest 的名字 + 候选的 marketplace），读不出名字的不算。
async fn candidate_ids(sources: &PluginSources) -> Vec<String> {
    let mut ids = Vec::new();
    for candidate in &sources.candidates {
        if let Ok(name) = manifest::name(&read_manifest(candidate.dir.as_str()).await) {
            ids.push(format!("{name}@{}", candidate.marketplace));
        }
    }
    ids
}

/// 读出的一个插件包；没加载的（依赖 Node 等）没有 `plugin`，只有 manifest 的事实。
struct Found<'a> {
    origin: Origin,
    manifest: Value,
    id: String,
    root: String,
    plugin: Option<&'a Plugin>,
}

/// 一次「读包」的加载：全部候选按启用加载（停用的也读组件），不建数据目录。
struct Loaded {
    set: PluginSet,
    sources: PluginSources,
    /// 数据根（执行侧，同会话的加载）。
    data_root: Option<String>,
}

impl Loaded {
    fn origin_of(&self, dir: &str) -> Origin {
        self.sources
            .candidates
            .iter()
            .find(|candidate| candidate.dir.as_str() == dir)
            .map_or(Origin::Directory, |candidate| origin(candidate.origin))
    }

    fn data_path(&self, id: &str) -> String {
        self.data_root
            .as_deref()
            .map(|root| {
                PathStyle::infer(root).join(root, &[&rust_cli_base::plugin::sanitize_id(id)])
            })
            .unwrap_or_default()
    }

    fn default_enabled(&self, origin: Origin, id: &str, manifest: &Value) -> bool {
        let origin = match origin {
            Origin::Marketplace => PluginOrigin::Installed,
            Origin::Builtin => PluginOrigin::Official,
            Origin::Directory => PluginOrigin::ConfigDir,
        };
        default_enabled(origin, id, manifest, &self.sources.official)
    }
}

impl PluginAdmin {
    /// 读包的加载（见文件头）。
    async fn load_packages(&self, context: &Context) -> Loaded {
        let plugins = &context.config.plugins;
        let side = EnvLayout::Controller.plugin_side(&plugins.root);
        let data_root = side.data_root.as_ref().map(|root| root.as_str().to_owned());
        let sources = sources(plugins, &context.cwd, side);
        let all = candidate_ids(&sources).await;
        let mut reading = sources.clone();
        reading.enabled_plugins = all.into_iter().map(|id| (id, true)).collect();
        reading.data_root = None;
        let set = load(
            self.env.as_ref(),
            &context.workspace,
            &[],
            (&reading, &self.unhosted),
        )
        .await;
        // 判定生效值用真的配置（`sources` 原样），读包只用改过的那份
        Loaded {
            set,
            sources,
            data_root,
        }
    }

    /// 读出的全部插件包：加载了的（全部启用的那次），与仍是独立的包、却没加载的（只有 manifest 的事实）。
    async fn packages<'a>(&self, loaded: &'a Loaded) -> Vec<Found<'a>> {
        let mut found = Vec::new();
        for plugin in loaded.set.plugins() {
            found.push(Found {
                origin: origin(plugin.origin),
                manifest: plugin.manifest.clone(),
                id: plugin.id().to_owned(),
                root: plugin.dir().to_owned(),
                plugin: Some(plugin),
            });
        }
        for skip in loaded.set.skipped() {
            let Some(id) = skip.id.as_ref().filter(|_| still_a_package(&skip.reason)) else {
                continue;
            };
            found.push(Found {
                origin: loaded.origin_of(&skip.dir),
                manifest: read_manifest(&skip.dir).await,
                id: id.clone(),
                root: skip.dir.clone(),
                plugin: None,
            });
        }
        found
    }

    /// 用户级的读（§12.1 `inventory`）：以 `cwd`（v5 给 home）为工作区读配置与存储。
    pub(crate) async fn inventory(&self, cwd: &str) -> Inventory {
        let context = self.context(cwd).await;
        let catalog = self
            .catalog_in(&context, &Control::new(CancellationToken::new()))
            .await;
        let loaded = self.load_packages(&context).await;
        let records: BTreeMap<String, String> = context
            .store
            .installed_records()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|record| (record.id, record.installed_at))
            .collect();
        let latest: BTreeMap<&str, &str> = catalog
            .available
            .iter()
            .filter_map(|entry| Some((entry.id.as_str(), entry.version.as_deref()?)))
            .collect();
        let user = config_object(&self.config_file(Scope::User, cwd)).await;
        let secrets = [&context.config.plugins.intents.secrets.user];
        let mut inventory = Inventory {
            diagnostics: catalog.diagnostics.clone(),
            ..Inventory::default()
        };
        for Found {
            origin,
            manifest,
            id,
            root,
            plugin,
        } in self.packages(&loaded).await
        {
            let (name, marketplace) = id.rsplit_once('@').unwrap_or((id.as_str(), ""));
            let hooks = plugin.map(hook_details).unwrap_or_default();
            let version = manifest.get("version").and_then(Value::as_str);
            let default_enabled = loaded.default_enabled(origin, &id, &manifest);
            let user = layer(&user, &secrets, &id, &manifest);
            inventory.packages.push(Package {
                name: name.to_owned(),
                marketplace: marketplace.to_owned(),
                origin,
                root,
                data_path: loaded.data_path(&id),
                components: match plugin {
                    Some(plugin) => components(plugin, &hooks).await,
                    None => Vec::new(),
                },
                mcp_servers: plugin
                    .map(|plugin| plugin.mcp_servers.iter().map(|s| s.name.clone()).collect())
                    .unwrap_or_default(),
                hooks,
                installed_at: records.get(&id).cloned(),
                latest_version: latest
                    .get(id.as_str())
                    .filter(|latest| version.is_some_and(|version| version != **latest))
                    .map(|latest| (*latest).to_owned()),
                default_enabled,
                enabled: user.enabled.unwrap_or(default_enabled),
                user,
                manifest,
                id,
            });
        }
        for error in loaded.set.errors() {
            inventory.diagnostics.push(
                Diagnostic::error(code::COMPONENT_PATH_INVALID, error.message.clone())
                    .of(error.plugin.clone()),
            );
        }
        inventory.catalog = catalog;
        inventory
    }

    /// 一个工作区的读（§12.1 `workspace`）：工作区层是 `<root>/.zcode/config.json` 与私有文件里这个工作区的键；生效值按这个工作区
    /// 合并后的配置算（用户级 → 自根向工作区根的项目层，与加载相同）。
    pub(crate) async fn workspace(&self, root: &str) -> WorkspaceFacts {
        let context = self.context(root).await;
        let loaded = self.load_packages(&context).await;
        let merged = &context.config.plugins.section.enabled_plugins;
        let config = config_object(&self.config_file(Scope::Workspace, root)).await;
        let key = project_key(root);
        let secrets: Vec<&SecretValues> = context
            .config
            .plugins
            .intents
            .secrets
            .workspaces
            .iter()
            .filter(|(path, _)| project_key(path) == key)
            .map(|(_, values)| values)
            .collect();
        let mut facts = WorkspaceFacts::default();
        for Found {
            origin,
            manifest,
            id,
            ..
        } in self.packages(&loaded).await
        {
            let default = loaded.default_enabled(origin, &id, &manifest);
            let enabled = merged.get(&id).copied().unwrap_or(default);
            let layer = layer(&config, &secrets, &id, &manifest);
            facts.plugins.push((id, layer, enabled));
        }
        // 启用了、这个运行时却不加载的（依赖 Node、前端没声明能力）：给原因（同 `zcode plugin list`，§15 第 9 项）
        for skip in loaded.set.skipped() {
            let Some(id) = &skip.id else { continue };
            let wanted = facts
                .plugins
                .iter()
                .any(|(plugin, _, on)| plugin == id && *on);
            if wanted
                && matches!(
                    skip.reason,
                    SkipReason::NodeRuntime(_) | SkipReason::FrontendLacks(_)
                )
            {
                facts.diagnostics.push(
                    Diagnostic::warning(
                        code::UNSUPPORTED_COMPONENT,
                        format!(
                            "Plugin is enabled but not loaded in this runtime: {}",
                            skip.reason.describe()
                        ),
                    )
                    .of(id.clone()),
                );
            }
        }
        facts
    }

    /// `describe`（§12.1）：已装的读包；没装的读市场缓存里的目录（本地来源）；都没有时组件为空加一条诊断。不联网、不写盘。
    pub(crate) async fn describe(
        &self,
        cwd: &str,
        plugin_id: &str,
    ) -> Result<Described, StoreError> {
        let inventory = self.inventory(cwd).await;
        if let Some(package) = inventory.packages.iter().find(|p| p.id == plugin_id) {
            return Ok(Described {
                components: package.components.clone(),
                manifest: package.manifest.clone(),
                diagnostics: Vec::new(),
            });
        }
        let id = PluginId::parse(plugin_id)
            .ok_or_else(|| StoreError::PluginNotFound(plugin_id.to_owned()))?;
        if !inventory
            .catalog
            .available
            .iter()
            .any(|entry| entry.id == plugin_id)
        {
            return Err(StoreError::PluginNotFound(plugin_id.to_owned()));
        }
        let context = self.context(cwd).await;
        let Some(dir) = context.store.local_entry_dir(&id).await? else {
            return Ok(Described {
                diagnostics: vec![
                    Diagnostic::warning(
                        code::VALIDATION_DEFERRED,
                        format!("The plugin content is not on this machine yet: {plugin_id}"),
                    )
                    .of(plugin_id.to_owned()),
                ],
                ..Described::default()
            });
        };
        let dir = dir.display().to_string();
        let one = PluginSources {
            candidates: vec![rust_cli_ext::plugins::PluginCandidate {
                dir: rust_cli_kernel_api::env::EnvPath::new(dir.as_str()),
                marketplace: id.marketplace.clone(),
                origin: PluginOrigin::Installed,
            }],
            enabled_plugins: [(plugin_id.to_owned(), true)].into(),
            ..PluginSources::default()
        };
        let set = load(
            self.env.as_ref(),
            &context.workspace,
            &[],
            (&one, &self.unhosted),
        )
        .await;
        let mut described = Described {
            manifest: read_manifest(&dir).await,
            ..Described::default()
        };
        match set.plugins().first() {
            Some(plugin) => described.components = components(plugin, &hook_details(plugin)).await,
            // 目录在、manifest 读不出（条目是 `strict: false` 时安装才合成 manifest）：组件为空，给原因
            None => described
                .diagnostics
                .extend(set.skipped().iter().map(|skip| {
                    Diagnostic::warning(code::MANIFEST_NOT_FOUND, skip.reason.describe())
                        .of(plugin_id.to_owned())
                })),
        }
        Ok(described)
    }
}
